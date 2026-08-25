use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dicom_viewer_core::{ViewerError, ViewerSourceIdentity, WorkspaceDocument};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::Builder;
use uuid::Uuid;

use super::DraftInteraction;

pub(in crate::app) const APP_ID: &str = "io.frames.dicom-viewer";
const REVISION_SCHEMA_VERSION: u32 = 1;
const MAX_REVISION_BYTES: u64 = 128 * 1024 * 1024;
const VALID_REVISIONS_TO_KEEP: usize = 5;
const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub(in crate::app) struct WorkspaceSaveRequest {
    document: Arc<WorkspaceDocument>,
    draft: Option<DraftInteraction>,
    captured_unix_ms: u64,
}

impl WorkspaceSaveRequest {
    #[must_use]
    pub(in crate::app) fn new(
        document: Arc<WorkspaceDocument>,
        draft: Option<DraftInteraction>,
    ) -> Self {
        Self {
            document,
            draft,
            captured_unix_ms: unix_time_millis(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedRevision {
    schema_version: u32,
    revision: u64,
    saved_unix_ms: u64,
    source_identity_digest: String,
    document: Arc<WorkspaceDocument>,
    draft: Option<DraftInteraction>,
}

#[derive(Debug, Clone)]
pub(in crate::app) struct RestoredWorkspace {
    revision: u64,
    saved_unix_ms: u64,
    path: PathBuf,
    document: Arc<WorkspaceDocument>,
    draft: Option<DraftInteraction>,
}

impl RestoredWorkspace {
    #[must_use]
    pub(in crate::app) const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub(in crate::app) const fn saved_unix_ms(&self) -> u64 {
        self.saved_unix_ms
    }

    #[cfg(test)]
    pub(in crate::app) fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub(in crate::app) fn document(&self) -> &WorkspaceDocument {
        &self.document
    }

    #[must_use]
    pub(in crate::app) fn draft(&self) -> Option<&DraftInteraction> {
        self.draft.as_ref()
    }
}

#[derive(Debug, Clone)]
pub(in crate::app) struct RevisionStore {
    root: PathBuf,
}

impl RevisionStore {
    #[must_use]
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(in crate::app) fn application_default() -> Result<Self, ViewerError> {
        let root = eframe::storage_dir(APP_ID).ok_or_else(|| {
            ViewerError::Unsupported(
                "this platform does not expose an application-data directory".into(),
            )
        })?;
        Ok(Self::new(root))
    }

    #[must_use]
    pub(in crate::app) fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub(super) fn revision_directory(&self, source: &ViewerSourceIdentity) -> PathBuf {
        self.root
            .join("workspaces")
            .join(source.digest())
            .join("revisions")
    }

    pub(super) fn save_revision(
        &self,
        request: &WorkspaceSaveRequest,
        revision: u64,
    ) -> Result<PathBuf, ViewerError> {
        request.document.validate()?;
        let serialized = SerializedRevision {
            schema_version: REVISION_SCHEMA_VERSION,
            revision,
            saved_unix_ms: request.captured_unix_ms,
            source_identity_digest: request.document.source_identity().digest().to_owned(),
            document: Arc::clone(&request.document),
            draft: request.draft.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&serialized).map_err(|error| {
            ViewerError::InvalidInput(format!("workspace revision could not be encoded: {error}"))
        })?;
        if bytes.len() as u64 > MAX_REVISION_BYTES {
            return Err(ViewerError::InvalidInput(
                "workspace revision exceeds the 128 MiB limit".into(),
            ));
        }
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let directory = self.revision_directory(request.document.source_identity());
        fs::create_dir_all(&directory).map_err(|source| io_error(&directory, source))?;
        let destination = directory.join(format!("rev-{revision:020}-{digest}.json"));
        if destination.exists() {
            let existing = read_bounded(&destination)?;
            if existing == bytes {
                return Ok(destination);
            }
            return Err(ViewerError::InvalidInput(
                "workspace revision destination already exists with different content".into(),
            ));
        }

        let mut temporary = Builder::new()
            .prefix(".workspace-revision-")
            .tempfile_in(&directory)
            .map_err(|source| io_error(&directory, source))?;
        temporary
            .as_file_mut()
            .write_all(&bytes)
            .and_then(|()| temporary.as_file_mut().flush())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|source| io_error(temporary.path(), source))?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = read_bounded(&destination)?;
                if existing != bytes {
                    return Err(io_error(&destination, error.error));
                }
            }
            Err(error) => return Err(io_error(&destination, error.error)),
        }
        self.prune_valid_revisions(request.document.source_identity())?;
        Ok(destination)
    }

    pub(in crate::app) fn restore_latest(
        &self,
        source: &ViewerSourceIdentity,
    ) -> Result<Option<RestoredWorkspace>, ViewerError> {
        Ok(self.valid_revisions(source)?.into_iter().next())
    }

    pub(super) fn valid_revisions(
        &self,
        source: &ViewerSourceIdentity,
    ) -> Result<Vec<RestoredWorkspace>, ViewerError> {
        let directory = self.revision_directory(source);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(&directory, error)),
        };
        let mut candidates = entries
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| {
                let revision = parse_revision_filename(&entry.file_name().to_string_lossy())?;
                Some((revision, entry.path()))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));

        let mut valid = Vec::new();
        for (filename_revision, path) in candidates {
            let Ok(restored) = read_revision(&path, filename_revision, source) else {
                continue;
            };
            valid.push(restored);
        }
        Ok(valid)
    }

    pub(super) fn next_revision(&self, source: &ViewerSourceIdentity) -> Result<u64, ViewerError> {
        Ok(self
            .valid_revisions(source)?
            .first()
            .map_or(1, |revision| revision.revision.saturating_add(1)))
    }

    pub(in crate::app) fn archive_current(
        &self,
        source: &ViewerSourceIdentity,
    ) -> Result<Option<PathBuf>, ViewerError> {
        let revisions = self.revision_directory(source);
        if !revisions.exists() {
            return Ok(None);
        }
        let source_root = revisions
            .parent()
            .expect("revision directories always have a source workspace parent");
        let archive_root = source_root.join("archive");
        fs::create_dir_all(&archive_root).map_err(|error| io_error(&archive_root, error))?;
        let destination = archive_root.join(format!(
            "{}-{}",
            unix_time_millis(),
            Uuid::new_v4().simple()
        ));
        fs::rename(&revisions, &destination).map_err(|error| io_error(&revisions, error))?;
        Ok(Some(destination))
    }

    pub(in crate::app) fn storage_usage_bytes(&self) -> Result<u64, ViewerError> {
        directory_size(&self.root)
    }

    pub(in crate::app) fn referenced_scheme_digests(&self) -> Result<HashSet<String>, ViewerError> {
        let root = self.root.join("workspaces");
        if !root.exists() {
            return Ok(HashSet::new());
        }
        let mut pending = vec![root];
        let mut digests = HashSet::new();
        while let Some(directory) = pending.pop() {
            let entries = fs::read_dir(&directory).map_err(|error| io_error(&directory, error))?;
            for entry in entries {
                let entry = entry.map_err(|error| io_error(&directory, error))?;
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Ok(bytes) = read_bounded(&path) else {
                    continue;
                };
                let Ok(revision) = serde_json::from_slice::<SerializedRevision>(&bytes) else {
                    continue;
                };
                if revision.document.validate().is_ok() {
                    digests.insert(revision.document.scheme().content_digest().to_owned());
                }
            }
        }
        Ok(digests)
    }

    pub(in crate::app) fn purge_archives_older_than(
        &self,
        retention: Duration,
    ) -> Result<usize, ViewerError> {
        let workspaces = self.root.join("workspaces");
        let entries = match fs::read_dir(&workspaces) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(io_error(&workspaces, error)),
        };
        let cutoff = SystemTime::now()
            .checked_sub(retention)
            .unwrap_or(UNIX_EPOCH);
        let mut removed = 0;
        for workspace in entries.filter_map(std::result::Result::ok) {
            let archive = workspace.path().join("archive");
            let Ok(archives) = fs::read_dir(&archive) else {
                continue;
            };
            for entry in archives.filter_map(std::result::Result::ok) {
                let old = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .is_ok_and(|modified| modified < cutoff);
                if old {
                    fs::remove_dir_all(entry.path())
                        .map_err(|error| io_error(entry.path(), error))?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Deletes only the workspace directory addressed by the supplied,
    /// content-derived source identity. Callers must obtain explicit user
    /// confirmation before invoking this operation.
    pub(in crate::app) fn delete_source_workspace(
        &self,
        source: &ViewerSourceIdentity,
    ) -> Result<bool, ViewerError> {
        let target = self.root.join("workspaces").join(source.digest());
        if !target.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&target).map_err(|error| io_error(&target, error))?;
        Ok(true)
    }

    /// Deletes the application-owned workspace storage root. Callers must
    /// obtain explicit user confirmation before invoking this operation.
    pub(in crate::app) fn delete_all_workspaces(&self) -> Result<bool, ViewerError> {
        let target = self.root.join("workspaces");
        if !target.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&target).map_err(|error| io_error(&target, error))?;
        Ok(true)
    }

    fn prune_valid_revisions(&self, source: &ViewerSourceIdentity) -> Result<(), ViewerError> {
        for revision in self
            .valid_revisions(source)?
            .into_iter()
            .skip(VALID_REVISIONS_TO_KEEP)
        {
            fs::remove_file(&revision.path).map_err(|error| io_error(&revision.path, error))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) enum AutosaveStatus {
    Clean,
    Pending,
    Saving,
    Saved { revision: u64, saved_unix_ms: u64 },
    Failed(String),
}

#[derive(Debug)]
struct SaveCompletion {
    revision: u64,
    saved_unix_ms: u64,
    result: Result<PathBuf, ViewerError>,
}

#[derive(Debug)]
pub(in crate::app) struct WorkspaceAutosave {
    store: RevisionStore,
    debounce: Duration,
    next_revision: u64,
    pending: Option<(WorkspaceSaveRequest, Instant)>,
    in_flight: Option<Receiver<SaveCompletion>>,
    status: AutosaveStatus,
}

impl WorkspaceAutosave {
    pub(in crate::app) fn new(
        store: RevisionStore,
        source: &ViewerSourceIdentity,
    ) -> Result<Self, ViewerError> {
        Ok(Self {
            next_revision: store.next_revision(source)?,
            store,
            debounce: DEFAULT_DEBOUNCE,
            pending: None,
            in_flight: None,
            status: AutosaveStatus::Clean,
        })
    }

    pub(in crate::app) fn queue(&mut self, request: WorkspaceSaveRequest, now: Instant) {
        self.pending = Some((request, now + self.debounce));
        if self.in_flight.is_none() {
            self.status = AutosaveStatus::Pending;
        }
    }

    pub(in crate::app) fn poll(&mut self, now: Instant) {
        self.poll_completion();
        let ready = self
            .pending
            .as_ref()
            .is_some_and(|(_, deadline)| now >= *deadline);
        if self.in_flight.is_none() && ready {
            self.start_pending();
        }
    }

    pub(in crate::app) fn flush(&mut self) -> Result<(), ViewerError> {
        if let Some(receiver) = self.in_flight.take() {
            let completion = receiver.recv().map_err(|_| {
                ViewerError::InvalidInput("workspace autosave worker exited".into())
            })?;
            self.apply_completion(completion)?;
        }
        if let Some((request, _)) = self.pending.take() {
            let revision = self.allocate_revision();
            self.store.save_revision(&request, revision)?;
            self.status = AutosaveStatus::Saved {
                revision,
                saved_unix_ms: request.captured_unix_ms,
            };
        }
        Ok(())
    }

    #[must_use]
    pub(in crate::app) fn status(&self) -> &AutosaveStatus {
        &self.status
    }

    #[must_use]
    pub(in crate::app) fn has_pending_write(&self) -> bool {
        self.pending.is_some() || self.in_flight.is_some()
    }

    fn start_pending(&mut self) {
        let Some((request, _)) = self.pending.take() else {
            return;
        };
        let store = self.store.clone();
        let revision = self.allocate_revision();
        let saved_unix_ms = request.captured_unix_ms;
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let result = store.save_revision(&request, revision);
            let _ = sender.send(SaveCompletion {
                revision,
                saved_unix_ms,
                result,
            });
        });
        self.in_flight = Some(receiver);
        self.status = AutosaveStatus::Saving;
    }

    fn poll_completion(&mut self) {
        let Some(receiver) = &self.in_flight else {
            return;
        };
        let completion = match receiver.try_recv() {
            Ok(completion) => completion,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => SaveCompletion {
                revision: self.next_revision.saturating_sub(1),
                saved_unix_ms: unix_time_millis(),
                result: Err(ViewerError::InvalidInput(
                    "workspace autosave worker exited".into(),
                )),
            },
        };
        self.in_flight = None;
        if let Err(error) = self.apply_completion(completion) {
            self.status = AutosaveStatus::Failed(error.to_string());
        } else if self.pending.is_some() {
            self.status = AutosaveStatus::Pending;
        }
    }

    fn apply_completion(&mut self, completion: SaveCompletion) -> Result<(), ViewerError> {
        completion.result?;
        self.status = AutosaveStatus::Saved {
            revision: completion.revision,
            saved_unix_ms: completion.saved_unix_ms,
        };
        Ok(())
    }

    fn allocate_revision(&mut self) -> u64 {
        let revision = self.next_revision;
        self.next_revision = self.next_revision.saturating_add(1);
        revision
    }
}

fn read_revision(
    path: &Path,
    filename_revision: u64,
    expected_source: &ViewerSourceIdentity,
) -> Result<RestoredWorkspace, ViewerError> {
    let bytes = read_bounded(path)?;
    let expected_digest = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('-'))
        .map(|(_, digest)| digest)
        .ok_or_else(|| ViewerError::InvalidInput("invalid workspace revision filename".into()))?;
    let actual_digest = format!("{:x}", Sha256::digest(&bytes));
    if expected_digest != actual_digest {
        return Err(ViewerError::InvalidInput(
            "workspace revision content digest does not match its filename".into(),
        ));
    }
    let serialized: SerializedRevision = serde_json::from_slice(&bytes).map_err(|error| {
        ViewerError::InvalidInput(format!("workspace revision is not valid JSON: {error}"))
    })?;
    if serialized.schema_version != REVISION_SCHEMA_VERSION
        || serialized.revision != filename_revision
        || serialized.source_identity_digest != expected_source.digest()
        || serialized.document.source_identity() != expected_source
    {
        return Err(ViewerError::InvalidInput(
            "workspace revision identity or version does not match".into(),
        ));
    }
    serialized.document.validate()?;
    Ok(RestoredWorkspace {
        revision: serialized.revision,
        saved_unix_ms: serialized.saved_unix_ms,
        path: path.to_path_buf(),
        document: serialized.document,
        draft: serialized.draft,
    })
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ViewerError> {
    let metadata = fs::metadata(path).map_err(|error| io_error(path, error))?;
    if metadata.len() > MAX_REVISION_BYTES {
        return Err(ViewerError::InvalidInput(
            "workspace revision exceeds the 128 MiB limit".into(),
        ));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        ViewerError::InvalidInput("workspace revision size does not fit this platform".into())
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    File::open(path)
        .and_then(|file| file.take(MAX_REVISION_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| io_error(path, error))?;
    if bytes.len() as u64 > MAX_REVISION_BYTES {
        return Err(ViewerError::InvalidInput(
            "workspace revision exceeds the 128 MiB limit".into(),
        ));
    }
    Ok(bytes)
}

fn parse_revision_filename(filename: &str) -> Option<u64> {
    let stem = filename.strip_prefix("rev-")?.strip_suffix(".json")?;
    let (revision, digest) = stem.split_once('-')?;
    if revision.len() != 20
        || digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    revision.parse().ok()
}

fn directory_size(path: &Path) -> Result<u64, ViewerError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(io_error(path, error)),
    };
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    let mut total = 0u64;
    for entry in fs::read_dir(path).map_err(|error| io_error(path, error))? {
        let entry = entry.map_err(|error| io_error(path, error))?;
        total = total.saturating_add(directory_size(&entry.path())?);
    }
    Ok(total)
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn io_error(path: impl AsRef<Path>, source: std::io::Error) -> ViewerError {
    ViewerError::Io {
        path: path.as_ref().to_path_buf(),
        source,
    }
}
