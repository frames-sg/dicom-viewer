use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use dicom_viewer_core::{AnnotationScheme, ViewerError};
use tempfile::Builder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum SchemeInstallOutcome {
    Installed,
    AlreadyInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemeOrigin {
    BuiltIn,
    Project,
}

#[derive(Debug, Clone)]
struct InstalledScheme {
    scheme: AnnotationScheme,
    origin: SchemeOrigin,
}

#[derive(Debug, Clone, Default)]
pub(in crate::app) struct SchemeLibrary {
    schemes: Vec<InstalledScheme>,
    storage_root: Option<PathBuf>,
}

impl SchemeLibrary {
    #[must_use]
    pub(in crate::app) fn with_builtins() -> Self {
        Self {
            schemes: vec![
                InstalledScheme {
                    scheme: AnnotationScheme::general_pathology_v1(),
                    origin: SchemeOrigin::BuiltIn,
                },
                InstalledScheme {
                    scheme: AnnotationScheme::tumor_mask_compatibility_v1(),
                    origin: SchemeOrigin::BuiltIn,
                },
            ],
            storage_root: None,
        }
    }

    pub(in crate::app) fn load_or_builtins(application_root: PathBuf) -> Result<Self, ViewerError> {
        let storage_root = application_root.join("annotation-schemes");
        fs::create_dir_all(&storage_root).map_err(|source| ViewerError::Io {
            path: storage_root.clone(),
            source,
        })?;
        let mut library = Self::with_builtins();
        library.storage_root = Some(storage_root.clone());
        let entries = fs::read_dir(&storage_root).map_err(|source| ViewerError::Io {
            path: storage_root.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| ViewerError::Io {
                path: storage_root.clone(),
                source,
            })?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let metadata = fs::metadata(&path).map_err(|source| ViewerError::Io {
                path: path.clone(),
                source,
            })?;
            if metadata.len() > 4 * 1024 * 1024 {
                return Err(ViewerError::InvalidInput(format!(
                    "installed annotation scheme exceeds 4 MiB: {}",
                    path.display()
                )));
            }
            let bytes = fs::read(&path).map_err(|source| ViewerError::Io {
                path: path.clone(),
                source,
            })?;
            let scheme = AnnotationScheme::from_json(&bytes)?;
            library.insert_project_scheme(scheme)?;
        }
        Ok(library)
    }

    pub(in crate::app) fn install_json(
        &mut self,
        json: &[u8],
    ) -> Result<SchemeInstallOutcome, ViewerError> {
        let scheme = AnnotationScheme::from_json(json)?;
        if let Some(existing) = self.schemes.iter().find(|existing| {
            existing.scheme.id() == scheme.id() && existing.scheme.version() == scheme.version()
        }) {
            if existing.scheme.content_digest() == scheme.content_digest() {
                return Ok(SchemeInstallOutcome::AlreadyInstalled);
            }
            return Err(ViewerError::InvalidInput(format!(
                "annotation scheme {} v{} is already installed with different content",
                scheme.id(),
                scheme.version()
            )));
        }
        if let Some(storage_root) = &self.storage_root {
            persist_scheme(storage_root, &scheme)?;
        }
        self.insert_project_scheme(scheme)?;
        Ok(SchemeInstallOutcome::Installed)
    }

    fn insert_project_scheme(&mut self, scheme: AnnotationScheme) -> Result<(), ViewerError> {
        if let Some(existing) = self.schemes.iter().find(|existing| {
            existing.scheme.id() == scheme.id() && existing.scheme.version() == scheme.version()
        }) {
            if existing.scheme.content_digest() == scheme.content_digest() {
                return Ok(());
            }
            return Err(ViewerError::InvalidInput(format!(
                "annotation scheme {} v{} is installed with conflicting content",
                scheme.id(),
                scheme.version()
            )));
        }
        self.schemes.push(InstalledScheme {
            scheme,
            origin: SchemeOrigin::Project,
        });
        self.schemes.sort_by(|left, right| {
            left.scheme
                .display_name()
                .cmp(right.scheme.display_name())
                .then_with(|| left.scheme.version().cmp(&right.scheme.version()))
        });
        Ok(())
    }

    pub(in crate::app) fn remove_project_scheme(
        &mut self,
        id: &str,
        version: u32,
        referenced_digests: &HashSet<String>,
    ) -> Result<(), ViewerError> {
        let index = self
            .schemes
            .iter()
            .position(|entry| entry.scheme.id() == id && entry.scheme.version() == version)
            .ok_or_else(|| {
                ViewerError::InvalidInput("annotation scheme is not installed".into())
            })?;
        let entry = &self.schemes[index];
        if entry.origin == SchemeOrigin::BuiltIn {
            return Err(ViewerError::InvalidInput(
                "built-in annotation schemes are immutable".into(),
            ));
        }
        if referenced_digests.contains(entry.scheme.content_digest()) {
            return Err(ViewerError::InvalidInput(
                "annotation scheme is referenced by a stored workspace".into(),
            ));
        }
        if let Some(storage_root) = &self.storage_root {
            archive_scheme(storage_root, entry.scheme.content_digest())?;
        }
        self.schemes.remove(index);
        Ok(())
    }

    pub(in crate::app) fn entries(&self) -> impl Iterator<Item = (&AnnotationScheme, bool)> {
        self.schemes
            .iter()
            .map(|entry| (&entry.scheme, entry.origin == SchemeOrigin::BuiltIn))
    }

    #[cfg(test)]
    pub(in crate::app) fn get(&self, id: &str, version: u32) -> Option<&AnnotationScheme> {
        self.schemes
            .iter()
            .find(|entry| entry.scheme.id() == id && entry.scheme.version() == version)
            .map(|entry| &entry.scheme)
    }
}

fn scheme_filename(content_digest: &str) -> String {
    format!("{}.json", content_digest.replace(':', "-"))
}

fn persist_scheme(storage_root: &Path, scheme: &AnnotationScheme) -> Result<(), ViewerError> {
    let destination = storage_root.join(scheme_filename(scheme.content_digest()));
    if destination.exists() {
        return Ok(());
    }
    let mut temporary = Builder::new()
        .prefix(".scheme-")
        .tempfile_in(storage_root)
        .map_err(|source| ViewerError::Io {
            path: storage_root.to_path_buf(),
            source,
        })?;
    let bytes = scheme.to_json()?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| ViewerError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .persist_noclobber(&destination)
        .map_err(|error| ViewerError::Io {
            path: destination,
            source: error.error,
        })?;
    Ok(())
}

fn archive_scheme(storage_root: &Path, content_digest: &str) -> Result<(), ViewerError> {
    let source = storage_root.join(scheme_filename(content_digest));
    if !source.exists() {
        return Ok(());
    }
    let removed = storage_root.join("removed");
    fs::create_dir_all(&removed).map_err(|source| ViewerError::Io {
        path: removed.clone(),
        source,
    })?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let destination = removed.join(format!("{timestamp}-{}", scheme_filename(content_digest)));
    fs::rename(&source, &destination).map_err(|source_error| ViewerError::Io {
        path: source,
        source: source_error,
    })
}
