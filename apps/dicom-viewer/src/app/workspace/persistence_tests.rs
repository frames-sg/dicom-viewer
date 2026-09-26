use std::fs;
use std::sync::Arc;
use std::time::Instant;

use dicom_viewer_core::{
    AnnotationScheme, Point2, VectorFindingGeometry, ViewerSourceIdentity, WorkspaceDocument,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::{
    AutosaveStatus, DraftInteraction, RevisionStore, WorkspaceAutosave, WorkspaceSaveRequest,
};

fn source(dataset: u128) -> ViewerSourceIdentity {
    ViewerSourceIdentity::new(dataset, 0, 0, 0, 0, 0, (2_000, 1_000))
}

fn document(dataset: u128) -> WorkspaceDocument {
    WorkspaceDocument::new(source(dataset), AnnotationScheme::general_pathology_v1()).unwrap()
}

#[test]
fn revision_store_restores_draft_and_falls_back_from_a_corrupt_newer_revision() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    let document = Arc::new(document(1));
    let layer = document.vector_layers()[0].id();
    let draft = DraftInteraction::vector_polygon(
        layer,
        "neoplasm",
        vec![Point2::new(1.0, 2.0), Point2::new(3.0, 4.0)],
    );
    store
        .save_revision(&WorkspaceSaveRequest::new(document, Some(draft.clone())), 7)
        .unwrap();

    let revision_dir = store.revision_directory(&source(1));
    fs::write(
        revision_dir.join("rev-00000000000000000008-deadbeef.json"),
        b"{broken",
    )
    .unwrap();

    let restored = store.restore_latest(&source(1)).unwrap().unwrap();
    assert_eq!(restored.revision(), 7);
    assert_eq!(restored.draft(), Some(&draft));
    assert_eq!(restored.document().source_identity(), &source(1));
}

#[test]
fn restore_loads_one_payload_and_autosave_reuses_its_revision() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    let request = WorkspaceSaveRequest::new(Arc::new(document(1)), None);
    for revision in 1..=5 {
        store.save_revision(&request, revision).unwrap();
    }
    let fresh = RevisionStore::new(temp.path().to_path_buf());
    let restored = fresh.restore_latest(&source(1)).unwrap();
    assert_eq!(fresh.revision_parse_count(), 1);
    let mut autosave = WorkspaceAutosave::from_restored(fresh.clone(), restored.as_ref());
    assert_eq!(fresh.revision_parse_count(), 1);
    autosave.queue(request, Instant::now());
    autosave.flush().unwrap();
    assert_eq!(
        fresh
            .restore_latest(&source(1))
            .unwrap()
            .unwrap()
            .revision(),
        6
    );
}

#[test]
fn revision_store_keeps_five_newest_valid_immutable_revisions() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    let mut document = document(2);
    let layer = document.vector_layers()[0].id();
    for revision in 1..=7 {
        document
            .add_vector_finding(
                layer,
                "cell",
                VectorFindingGeometry::Point(Point2::new(revision as f64, 10.0)),
            )
            .unwrap();
        store
            .save_revision(
                &WorkspaceSaveRequest::new(Arc::new(document.clone()), None),
                revision,
            )
            .unwrap();
    }
    let revisions = store.valid_revisions(&source(2)).unwrap();
    assert_eq!(revisions.len(), 5);
    assert_eq!(revisions.first().unwrap().revision(), 7);
    assert_eq!(revisions.last().unwrap().revision(), 3);
}

#[test]
fn pruning_reuses_verified_semantics_but_still_checks_changed_revision_bytes() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    let request = WorkspaceSaveRequest::new(Arc::new(document(72)), None);
    let first = store.save_revision(&request, 1).unwrap();
    let before = store.revision_parse_count();
    let second = store.save_revision(&request, 2).unwrap();
    assert_eq!(
        store.revision_parse_count(),
        before,
        "known, byte-verified revisions should not repeat JSON/geometry validation during pruning"
    );
    fs::write(&second, b"corrupt").unwrap();
    let restored = store.restore_latest(&source(72)).unwrap().unwrap();
    assert_eq!(restored.path(), first);
    assert_eq!(restored.revision(), 1);
}

#[test]
fn restore_rejects_identity_mismatch_without_partial_application() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    store
        .save_revision(&WorkspaceSaveRequest::new(Arc::new(document(3)), None), 1)
        .unwrap();
    assert!(store.restore_latest(&source(4)).unwrap().is_none());

    let wrong_directory = store.revision_directory(&source(4));
    fs::create_dir_all(&wrong_directory).unwrap();
    let saved = store.valid_revisions(&source(3)).unwrap();
    let filename = saved[0].path().file_name().unwrap();
    fs::copy(saved[0].path(), wrong_directory.join(filename)).unwrap();
    assert!(store.restore_latest(&source(4)).unwrap().is_none());
}

#[test]
fn restore_rejects_duplicate_json_keys_even_when_the_content_digest_matches() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    let saved = store
        .save_revision(&WorkspaceSaveRequest::new(Arc::new(document(44)), None), 1)
        .unwrap();
    let bytes = fs::read(&saved).unwrap();
    let mut duplicate = br#"{"schema_version":1,"#.to_vec();
    duplicate.extend_from_slice(&bytes[1..]);
    let digest = format!("{:x}", Sha256::digest(&duplicate));
    let duplicate_path = saved
        .parent()
        .unwrap()
        .join(format!("rev-00000000000000000002-{digest}.json"));
    fs::write(&duplicate_path, duplicate).unwrap();

    assert_eq!(
        store
            .restore_latest(&source(44))
            .unwrap()
            .unwrap()
            .revision(),
        1
    );
}

#[test]
fn start_fresh_archives_revisions_instead_of_deleting_them() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    store
        .save_revision(&WorkspaceSaveRequest::new(Arc::new(document(5)), None), 1)
        .unwrap();
    let archive = store.archive_current(&source(5)).unwrap().unwrap();
    assert!(archive.exists());
    assert!(store.restore_latest(&source(5)).unwrap().is_none());
    assert_eq!(fs::read_dir(archive).unwrap().count(), 1);
}

#[test]
fn revision_filenames_are_content_addressed_and_never_overwritten() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    let request = WorkspaceSaveRequest::new(Arc::new(document(6)), None);
    let first = store.save_revision(&request, 1).unwrap();
    let second = store.save_revision(&request, 1).unwrap();
    assert_eq!(first, second);
    assert_eq!(fs::read_dir(first.parent().unwrap()).unwrap().count(), 1);
    assert!(first
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("rev-00000000000000000001-"));
}

#[test]
fn confirmed_storage_deletion_is_scoped_to_one_source_or_workspace_root() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    for dataset in [7, 8] {
        store
            .save_revision(
                &WorkspaceSaveRequest::new(Arc::new(document(dataset)), None),
                1,
            )
            .unwrap();
    }
    assert!(store.delete_source_workspace(&source(7)).unwrap());
    assert!(store.restore_latest(&source(7)).unwrap().is_none());
    assert!(store.restore_latest(&source(8)).unwrap().is_some());
    assert!(store.delete_all_workspaces().unwrap());
    assert!(store.restore_latest(&source(8)).unwrap().is_none());
}

#[test]
fn autosave_coalesces_pending_changes_to_the_latest_snapshot_and_flushes() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    let mut autosave = WorkspaceAutosave::new(store.clone(), &source(9)).unwrap();
    let mut latest = document(9);
    autosave.queue(
        WorkspaceSaveRequest::new(Arc::new(latest.clone()), None),
        Instant::now(),
    );
    let layer = latest.vector_layers()[0].id();
    latest
        .add_vector_finding(
            layer,
            "cell",
            VectorFindingGeometry::Point(Point2::new(9.0, 9.0)),
        )
        .unwrap();
    autosave.queue(
        WorkspaceSaveRequest::new(Arc::new(latest), None),
        Instant::now(),
    );

    autosave.flush().unwrap();
    let revisions = store.valid_revisions(&source(9)).unwrap();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].document().object_count(), 1);
    assert!(!autosave.has_pending_write());
}

#[test]
fn autosave_retries_failed_writes_without_losing_the_latest_snapshot() {
    for (background, poll_failure, newer_edit) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (true, true, true),
    ] {
        let temp = tempdir().unwrap();
        let root = temp.path().join("workspace");
        // A regular file prevents the revision store from creating its directories.
        fs::write(&root, b"blocked").unwrap();
        let store = RevisionStore::new(root.clone());
        let mut autosave = WorkspaceAutosave::from_restored(store.clone(), None);
        let mut latest = document(9);
        let layer = latest.vector_layers()[0].id();
        latest
            .add_vector_finding(
                layer,
                "cell",
                VectorFindingGeometry::Point(Point2::new(9.0, 9.0)),
            )
            .unwrap();
        autosave.queue(
            WorkspaceSaveRequest::new(Arc::new(latest.clone()), None),
            Instant::now(),
        );
        if background {
            autosave.poll(Instant::now() + std::time::Duration::from_secs(2));
        }
        if newer_edit {
            latest
                .add_vector_finding(
                    layer,
                    "cell",
                    VectorFindingGeometry::Point(Point2::new(10.0, 9.0)),
                )
                .unwrap();
            autosave.queue(
                WorkspaceSaveRequest::new(Arc::new(latest.clone()), None),
                Instant::now(),
            );
        }
        if poll_failure {
            let deadline = Instant::now() + std::time::Duration::from_secs(2);
            while !matches!(autosave.status(), AutosaveStatus::Failed(_)) {
                assert!(Instant::now() < deadline, "save worker did not complete");
                autosave.poll(Instant::now());
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }

        assert!(autosave.flush().is_err());
        assert!(matches!(autosave.status(), AutosaveStatus::Failed(_)));
        assert!(autosave.has_pending_write());
        assert!(
            autosave.flush().is_err(),
            "retry must attempt the write again"
        );

        fs::rename(&root, temp.path().join("former-blocker")).unwrap();
        autosave.flush().unwrap();

        let restored = store.restore_latest(&source(9)).unwrap().unwrap();
        assert_eq!(restored.document().object_count(), latest.object_count());
        assert_eq!(restored.document().revision(), latest.revision());
        assert!(!autosave.has_pending_write());
        assert!(matches!(autosave.status(), AutosaveStatus::Saved { .. }));
    }
}

#[test]
fn restore_skips_an_oversized_revision_before_parsing() {
    let temp = tempdir().unwrap();
    let store = RevisionStore::new(temp.path().to_path_buf());
    let directory = store.revision_directory(&source(10));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("rev-00000000000000000001-{}.json", "0".repeat(64)));
    let file = fs::File::create(path).unwrap();
    file.set_len(128 * 1024 * 1024 + 1).unwrap();

    assert!(store.restore_latest(&source(10)).unwrap().is_none());
}
