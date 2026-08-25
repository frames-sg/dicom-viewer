use std::process::Command;

#[test]
fn annotation_probe_process_preserves_json_stdout_and_usage_exit_contracts() {
    let probe = env!("CARGO_BIN_EXE_annotation_probe");

    let recognized = Command::new(probe)
        .arg("convert-raster")
        .output()
        .expect("annotation_probe should start");
    assert_eq!(recognized.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&recognized.stdout).unwrap();
    assert_eq!(report["schema"], "conversion-report-v1");
    assert_eq!(report["status"], "error");
    assert_eq!(report["error"]["code"], "USAGE_ERROR");
    assert!(String::from_utf8(recognized.stderr)
        .unwrap()
        .contains("--source is required"));

    let unknown = Command::new(probe)
        .arg("unsupported-command")
        .output()
        .expect("annotation_probe should start");
    assert_eq!(unknown.status.code(), Some(2));
    assert!(unknown.stdout.is_empty());
    let stderr = String::from_utf8(unknown.stderr).unwrap();
    assert!(stderr.contains("first argument must name a supported command"));
    assert!(stderr.contains("annotation_probe convert-geojson"));

    let directory = tempfile::tempdir().unwrap();
    let missing_source = directory.path().join("missing-source.dcm");
    let geojson_output = directory.path().join("ann.dcm");
    let raster_output = directory.path().join("pm.dcm");
    let conversion_cases = [
        vec![
            "convert-geojson".into(),
            "--source".into(),
            missing_source.as_os_str().into(),
            "--mapping".into(),
            directory
                .path()
                .join("missing-mapping.json")
                .into_os_string(),
            "--coordinate-space".into(),
            "level0-pixels".into(),
            "--target".into(),
            "ann".into(),
            "--output".into(),
            geojson_output.as_os_str().into(),
            directory.path().join("missing.geojson").into_os_string(),
        ],
        vec![
            "convert-raster".into(),
            "--source".into(),
            missing_source.as_os_str().into(),
            "--profile".into(),
            directory
                .path()
                .join("missing-profile.json")
                .into_os_string(),
            "--output".into(),
            raster_output.as_os_str().into(),
            directory.path().join("missing.npy").into_os_string(),
        ],
    ];
    for arguments in conversion_cases {
        let failed = Command::new(probe)
            .args(arguments)
            .output()
            .expect("annotation_probe conversion should start");
        assert_eq!(failed.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
        assert_eq!(report["schema"], "conversion-report-v1");
        assert_eq!(report["status"], "error");
        assert_eq!(report["error"]["code"], "SOURCE_READ_FAILED");
        assert!(!failed.stderr.is_empty());
    }
    assert!(!geojson_output.exists());
    assert!(!raster_output.exists());
}
