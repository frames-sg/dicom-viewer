use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Instant;

use dicom_dictionary_std::tags;
use dicom_object::{file::ReadPreamble, OpenFileOptions};

#[test]
#[ignore = "manual parser characterization; requires --release and DICOM_VIEWER_WSI_FIXTURE pointing at a DICOM file"]
fn cpu_metadata_parser_release_characterization() {
    if cfg!(debug_assertions) {
        panic!("run this characterization with --release");
    }
    let path = PathBuf::from(
        std::env::var_os("DICOM_VIEWER_WSI_FIXTURE")
            .expect("DICOM_VIEWER_WSI_FIXTURE must point at a trusted local DICOM file"),
    );
    // Admit the fixture with the complete production preflight first. The timed
    // reads below isolate eager parsing; they are not replacements for preflight.
    let expected = crate::inspection::open_metadata_object(&path).unwrap();
    let mut expected_bytes = Vec::new();
    expected.write_all(&mut expected_bytes).unwrap();
    for sample in 1..=5 {
        let started = Instant::now();
        let admitted = crate::inspection::open_metadata_object(&path).unwrap();
        let admitted_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let mut admitted_bytes = Vec::new();
        admitted.write_all(&mut admitted_bytes).unwrap();
        assert!(
            admitted_bytes == expected_bytes,
            "production metadata differs between samples"
        );
        eprintln!(
            "{}",
            serde_json::json!({
                "workload": "cpu-metadata-production",
                "sample": sample,
                "admitted_ms": admitted_ms,
            })
        );
        // Alternate order to reduce a fixed-order filesystem-cache bias.
        for buffered in if sample % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let file = File::open(&path).unwrap();
            let options = OpenFileOptions::new()
                .read_until(tags::FLOAT_PIXEL_DATA)
                .read_preamble(ReadPreamble::Always);
            let started = Instant::now();
            let actual = if buffered {
                options.from_reader(BufReader::new(file))
            } else {
                options.from_reader(file)
            }
            .unwrap();
            let parse_ms = started.elapsed().as_secs_f64() * 1_000.0;
            // DICOM undefined lengths compare unequal even to themselves.
            // Compare the complete emitted metadata instead, outside the timer.
            let mut actual_bytes = Vec::new();
            actual.write_all(&mut actual_bytes).unwrap();
            assert!(
                actual_bytes == expected_bytes,
                "metadata parser output differs from the admitted fixture"
            );
            eprintln!(
                "{}",
                serde_json::json!({
                    "workload": "cpu-metadata-parser",
                    "sample": sample,
                    "buffered": buffered,
                    "parse_ms": parse_ms,
                })
            );
        }
    }
}
