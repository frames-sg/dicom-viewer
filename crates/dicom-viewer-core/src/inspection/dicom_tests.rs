use super::*;

fn part10_file_meta(transfer_syntax: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 128];
    bytes.extend_from_slice(b"DICM");
    bytes.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, b'U', b'L', 0x04, 0x00]);
    let group_length = u32::try_from(8 + transfer_syntax.len()).unwrap();
    bytes.extend_from_slice(&group_length.to_le_bytes());
    bytes.extend_from_slice(&[0x02, 0x00, 0x10, 0x00, b'U', b'I']);
    bytes.extend_from_slice(&u16::try_from(transfer_syntax.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(transfer_syntax);
    bytes
}

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
fn explicit_headers_accept_both_length_forms_and_reject_malformed_headers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("header.bin");

    std::fs::write(&path, [0x02, 0x00, 0x00, 0x00, b'U', b'L', 0x04, 0x00]).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    let header = read_explicit_header(&mut file, &path).unwrap();
    assert_eq!(header.tag, tags::FILE_META_INFORMATION_GROUP_LENGTH);
    assert_eq!(header.vr, VR::UL);
    assert_eq!((header.value_len, header.header_len), (4, 8));

    std::fs::write(
        &path,
        [
            0x02, 0x00, 0x01, 0x00, b'O', b'B', 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
        ],
    )
    .unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    let header = read_explicit_header(&mut file, &path).unwrap();
    assert_eq!(
        (header.vr, header.value_len, header.header_len),
        (VR::OB, 4, 12)
    );

    for malformed in [
        vec![0x02, 0x00, 0x01, 0x00, b'Z', b'Z', 0x00, 0x00],
        vec![0x02, 0x00, 0x01, 0x00, b'O', b'B', 0x01, 0x00],
        vec![0x02, 0x00],
    ] {
        std::fs::write(&path, malformed).unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        assert!(read_explicit_header(&mut file, &path).is_err());
    }
}

#[test]
fn file_meta_preflight_rejects_missing_invalid_and_unsupported_transfer_syntaxes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("meta.dcm");

    let mut missing = vec![0; 128];
    missing.extend_from_slice(b"DICM");
    missing.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, b'U', b'L', 0x04, 0x00]);
    missing.extend_from_slice(&0_u32.to_le_bytes());
    std::fs::write(&path, missing).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    assert!(preflight_file_meta(&mut file, &path).is_err());

    std::fs::write(&path, part10_file_meta(&[0xff, 0x00])).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    assert!(preflight_file_meta(&mut file, &path).is_err());

    std::fs::write(&path, part10_file_meta(b"9.9\0")).unwrap();
    assert!(open_metadata_object(&path).is_err());

    let mut truncated_value = part10_file_meta(b"1.2.840.10008.1.2.1\0");
    truncated_value.extend_from_slice(&[0x10, 0x00, 0x10, 0x00, b'P', b'N', 0x04, 0x00]);
    std::fs::write(&path, truncated_value).unwrap();
    assert!(open_metadata_object(&path).is_err());

    let mut invalid_data_set_vr = part10_file_meta(b"1.2.840.10008.1.2.1\0");
    invalid_data_set_vr.extend_from_slice(&[0x10, 0x00, 0x10, 0x00, b'Z', b'Z', 0x00, 0x00]);
    std::fs::write(&path, invalid_data_set_vr).unwrap();
    assert!(open_metadata_object(&path).is_err());

    let mut oversized = vec![0; 128];
    oversized.extend_from_slice(b"DICM");
    oversized.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, b'U', b'L', 0x04, 0x00]);
    oversized.extend_from_slice(&(MAX_FILE_META_BYTES + 1).to_le_bytes());
    std::fs::write(&path, oversized).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    assert!(preflight_file_meta(&mut file, &path).is_err());
}

#[test]
fn real_wsi_metadata_exposes_numeric_spacing_and_pixel_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dcm");
    crate::annotation_test_support::write_source_wsi(&source, 16, 12, 4, 4);

    let object = open_metadata_object(&source).unwrap();
    assert_eq!(optional_u32(&object, tags::ROWS), Some(4));
    assert_eq!(optional_spacing(&object), Some((0.00025, 0.00025)));
    assert!(is_pixel_value(tags::PIXEL_DATA));
    assert!(is_pixel_value(tags::FLOAT_PIXEL_DATA));
    assert!(is_pixel_value(tags::DOUBLE_FLOAT_PIXEL_DATA));
    assert!(!is_pixel_value(tags::ROWS));

    let inspection = inspect_input(&source).unwrap();
    assert_eq!(inspection.instances.len(), 1);
    assert_eq!(inspection.instances[0].rows, Some(4));
    assert_eq!(file_name_display(&source), "source.dcm");
    assert!(invalid_metadata(&source, "reason")
        .to_string()
        .contains("reason"));
}

#[cfg(unix)]
#[test]
fn non_utf8_file_names_remain_diagnostic() {
    use std::os::unix::ffi::OsStringExt;

    let path = PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]));
    assert!(!file_name_display(&path).is_empty());
}
