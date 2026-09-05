use super::*;

#[test]
fn path_discovery_is_sorted_bounded_and_distinguishes_dicom_names() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("a.dcm");
    let second = directory.path().join("b.DICOM");
    let ignored_directory = directory.path().join("c.dcm");
    std::fs::write(&second, b"second").unwrap();
    std::fs::write(&first, b"first").unwrap();
    std::fs::create_dir(&ignored_directory).unwrap();

    assert!(likely_dicom_path(&first));
    assert!(likely_dicom_path(&second));
    assert!(!likely_dicom_path(&directory.path().join("plain.txt")));
    assert_eq!(
        candidate_paths_with_limit(&first, SourceKind::File, 0).unwrap(),
        vec![first.clone()]
    );
    assert_eq!(
        candidate_paths(directory.path(), SourceKind::Folder).unwrap(),
        vec![first.clone(), second]
    );
    assert!(candidate_paths_with_limit(directory.path(), SourceKind::Folder, 1).is_err());
    assert!(
        candidate_paths_with_limit(&directory.path().join("missing"), SourceKind::Folder, 1,)
            .is_err()
    );
}

#[test]
fn preamble_and_top_level_inspection_fail_closed_without_hiding_plain_files() {
    let directory = tempfile::tempdir().unwrap();
    let short = directory.path().join("plain.txt");
    std::fs::write(&short, b"plain").unwrap();
    assert!(!has_dicom_preamble(&short).unwrap());
    let inspection = inspect_input(&short).unwrap();
    assert_eq!(inspection.source_kind, SourceKind::File);
    assert_eq!(inspection.file_count, 1);
    assert!(inspection.instances.is_empty());
    assert!(inspection.warnings.is_empty());

    let preamble = directory.path().join("preamble.bin");
    let mut bytes = vec![0; 128];
    bytes.extend_from_slice(b"DICM");
    std::fs::write(&preamble, bytes).unwrap();
    assert!(has_dicom_preamble(&preamble).unwrap());
    assert!(has_dicom_preamble(&directory.path().join("missing")).is_err());
    assert!(has_dicom_preamble(directory.path()).is_err());
    assert!(inspect_input(&directory.path().join("missing")).is_err());

    let invalid = directory.path().join("invalid.dcm");
    std::fs::write(&invalid, b"not DICOM").unwrap();
    let inspection = inspect_input(&invalid).unwrap();
    assert!(inspection.instances.is_empty());
    assert_eq!(inspection.warnings.len(), 1);
    assert!(inspection.warnings[0].contains("ignored unreadable DICOM candidate"));
}

#[test]
fn real_wsi_metadata_exposes_numeric_spacing_and_pixel_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dcm");
    crate::annotation_test_support::write_source_wsi(&source, 16, 12, 4, 4);

    let object = open_metadata_object(&source).unwrap();
    assert_eq!(optional_u32(&object, tags::ROWS), Some(4));
    assert_eq!(optional_spacing(&object), Some((0.00025, 0.00025)));

    let inspection = inspect_input(&source).unwrap();
    assert_eq!(inspection.instances.len(), 1);
    assert_eq!(inspection.instances[0].rows, Some(4));
    assert_eq!(file_name_display(&source), "source.dcm");
}

#[cfg(unix)]
#[test]
fn non_utf8_file_names_remain_diagnostic() {
    use std::os::unix::ffi::OsStringExt;

    let path = PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]));
    assert!(!file_name_display(&path).is_empty());
}
