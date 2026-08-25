use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use dicom_viewer_core::frames_viewer_producer;
use wsi_dicom_annotations::{
    DicomAnnotationContext, DicomBundlePublication, DicomSinglePublication, PathologyAnnotationSet,
    PathologyCoordinateSpace, PathologyDicomDocuments, PathologyDicomTarget as Target,
    PathologyDocumentWriteError,
};

use crate::command::{next_path, next_utf8, set_once, USAGE};
use crate::conversion_report::{
    emit_error, emit_success, emit_usage_error, write_manifest, ConversionError, CoverageReport,
    InputReport, OutputReport, SuccessReport,
};
use crate::publication::{publication_error, read_bounded_file};
use crate::PEAK_ALLOC;

const MAX_GEOJSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MAPPING_BYTES: u64 = 4 * 1024 * 1024;
const ANN_STORAGE_UID: &str = "1.2.840.10008.5.1.4.1.1.91.1";
const SEG_STORAGE_UID: &str = "1.2.840.10008.5.1.4.1.1.66.4";
const COMPREHENSIVE_3D_SR_STORAGE_UID: &str = "1.2.840.10008.5.1.4.1.1.88.34";

#[derive(Debug)]
pub(crate) struct Arguments {
    pub(crate) source: PathBuf,
    pub(crate) canonical_source: PathBuf,
    pub(crate) mapping: PathBuf,
    pub(crate) coordinate_space: PathologyCoordinateSpace,
    pub(crate) targets: Vec<Target>,
    pub(crate) output: Option<PathBuf>,
    pub(crate) output_dir: Option<PathBuf>,
    pub(crate) allow_lossy: bool,
    pub(crate) input: PathBuf,
}

pub(crate) fn execute(
    arguments: impl IntoIterator<Item = OsString>,
    stdout: impl Write,
    stderr: impl Write,
) -> i32 {
    let arguments = match parse(arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            return emit_usage_error("convert-geojson", &error, USAGE, stdout, stderr);
        }
    };
    match run(&arguments) {
        Ok(report) => emit_success(&report, stdout),
        Err(error) => emit_error("convert-geojson", &error, stdout, stderr),
    }
}

pub(crate) fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let mut arguments = arguments.into_iter();
    let mut source = None;
    let mut canonical_source = None;
    let mut mapping = None;
    let mut coordinate_space = None;
    let mut targets = Vec::new();
    let mut output = None;
    let mut output_dir = None;
    let mut allow_lossy = false;
    let mut input = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--source") => {
                let value = next_path(&mut arguments, "--source")?;
                set_once(&mut source, value, "--source")?;
            }
            Some("--canonical-source") => {
                let value = next_path(&mut arguments, "--canonical-source")?;
                set_once(&mut canonical_source, value, "--canonical-source")?;
            }
            Some("--mapping") => {
                let value = next_path(&mut arguments, "--mapping")?;
                set_once(&mut mapping, value, "--mapping")?;
            }
            Some("--coordinate-space") => {
                let value = next_utf8(&mut arguments, "--coordinate-space")?;
                let parsed = match value.as_str() {
                    "level0-pixels" => PathologyCoordinateSpace::Level0Pixels,
                    "source-pixels" => PathologyCoordinateSpace::SourcePixels,
                    "slide-mm" => PathologyCoordinateSpace::SlideMillimeters,
                    _ => {
                        return Err(
                            "--coordinate-space requires level0-pixels, source-pixels, or slide-mm"
                                .into(),
                        );
                    }
                };
                set_once(&mut coordinate_space, parsed, "--coordinate-space")?;
            }
            Some("--target") => {
                let value = next_utf8(&mut arguments, "--target")?;
                let target = match value.as_str() {
                    "ann" => Target::Ann,
                    "seg" => Target::Seg,
                    "sr" => Target::Sr,
                    _ => return Err("--target requires ann, seg, or sr".into()),
                };
                if targets.contains(&target) {
                    return Err(format!("duplicate --target {value}"));
                }
                targets.push(target);
            }
            Some("--output") => {
                let value = next_path(&mut arguments, "--output")?;
                set_once(&mut output, value, "--output")?;
            }
            Some("--output-dir") => {
                let value = next_path(&mut arguments, "--output-dir")?;
                set_once(&mut output_dir, value, "--output-dir")?;
            }
            Some("--allow-lossy") => allow_lossy = true,
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option {value}"));
            }
            _ if input.is_none() => input = Some(PathBuf::from(argument)),
            _ => return Err("only one GeoJSON input path may be supplied".into()),
        }
    }
    let source = source.ok_or_else(|| "--source is required".to_string())?;
    let mapping = mapping.ok_or_else(|| "--mapping is required".to_string())?;
    let coordinate_space =
        coordinate_space.ok_or_else(|| "--coordinate-space is required".to_string())?;
    if targets.is_empty() {
        return Err("at least one --target is required".into());
    }
    validate_destination(targets.len(), output.as_ref(), output_dir.as_ref())?;
    let input = input.ok_or_else(|| "a GeoJSON input path is required".to_string())?;
    Ok(Arguments {
        canonical_source: canonical_source.unwrap_or_else(|| source.clone()),
        source,
        mapping,
        coordinate_space,
        targets,
        output,
        output_dir,
        allow_lossy,
        input,
    })
}

fn validate_destination(
    target_count: usize,
    output: Option<&PathBuf>,
    output_dir: Option<&PathBuf>,
) -> Result<(), String> {
    match (target_count, output, output_dir) {
        (1, Some(_), None) | (2.., None, Some(_)) => Ok(()),
        (1, None, None) => Err("one target requires --output".into()),
        (2.., None, None) => Err("multiple targets require --output-dir".into()),
        (1, None, Some(_)) => Err("one target requires --output, not --output-dir".into()),
        (2.., Some(_), None) => Err("multiple targets require --output-dir, not --output".into()),
        (_, Some(_), Some(_)) => Err("--output and --output-dir are mutually exclusive".into()),
        (0, _, _) => unreachable!("target count was validated"),
    }
}

fn run(arguments: &Arguments) -> Result<SuccessReport, ConversionError> {
    PEAK_ALLOC.reset_peak_usage();
    let started = Instant::now();
    let source = DicomAnnotationContext::from_source(&arguments.source).map_err(|error| {
        ConversionError::new(
            "SOURCE_READ_FAILED",
            format!("source WSI could not be read: {error}"),
        )
    })?;
    let canonical_source = DicomAnnotationContext::from_source(&arguments.canonical_source)
        .map_err(|error| {
            ConversionError::new(
                "CANONICAL_SOURCE_READ_FAILED",
                format!("canonical source WSI could not be read: {error}"),
            )
        })?;
    let geojson = read_bounded_file(&arguments.input, MAX_GEOJSON_BYTES, "GeoJSON input")?;
    let mapping = read_bounded_file(&arguments.mapping, MAX_MAPPING_BYTES, "mapping profile")?;
    let annotations = PathologyAnnotationSet::from_json(
        &geojson,
        &mapping,
        &source,
        &canonical_source,
        arguments.coordinate_space,
        arguments.allow_lossy,
    )
    .map_err(ConversionError::conversion)?;
    let documents = PathologyDicomDocuments::build(&annotations, &arguments.targets)
        .map_err(ConversionError::conversion)?
        .with_producers(
            frames_viewer_producer(9101, "WSI annotations").map_err(ConversionError::conversion)?,
            frames_viewer_producer(9201, "WSI segmentations")
                .map_err(ConversionError::conversion)?,
            frames_viewer_producer(9301, "WSI measurement reports")
                .map_err(ConversionError::conversion)?,
        );
    let mut report = base_report(arguments, &annotations, &mapping, &geojson, started)?;
    let protected = [
        arguments.source.as_path(),
        arguments.canonical_source.as_path(),
        arguments.mapping.as_path(),
        arguments.input.as_path(),
    ];
    if let Some(output) = &arguments.output {
        let target = arguments.targets[0];
        let publication = DicomSinglePublication::new(output, target.file_name(), &protected)
            .map_err(publication_error)?;
        documents
            .write_and_verify(target, publication.staged_file(), &source)
            .map_err(pathology_write_error)?;
        report.outputs.push(output_report(
            &documents,
            target,
            publication.staged_file(),
            publication.destination(),
        )?);
        report.record_verification_completion(started, PEAK_ALLOC.peak_usage());
        publication.publish().map_err(publication_error)?;
    } else if let Some(output_dir) = &arguments.output_dir {
        let publication =
            DicomBundlePublication::new(output_dir, &protected).map_err(publication_error)?;
        for &target in &arguments.targets {
            let staged = publication.staging_path().join(target.file_name());
            documents
                .write_and_verify(target, &staged, &source)
                .map_err(pathology_write_error)?;
            report.outputs.push(output_report(
                &documents,
                target,
                &staged,
                &publication.destination().join(target.file_name()),
            )?);
        }
        report.record_verification_completion(started, PEAK_ALLOC.peak_usage());
        let manifest = publication.staging_path().join("manifest.json");
        write_manifest(&manifest, &report)?;
        publication
            .sync_staged_file(&manifest)
            .map_err(publication_error)?;
        publication.publish().map_err(publication_error)?;
    } else {
        return Err(ConversionError::new(
            "INVALID_DESTINATION",
            "conversion has no destination",
        ));
    }
    Ok(report)
}

fn output_report(
    documents: &PathologyDicomDocuments,
    target: Target,
    staged_path: &std::path::Path,
    report_path: &std::path::Path,
) -> Result<OutputReport, ConversionError> {
    let (sop_class_uid, sop_instance_uid, series_instance_uid) = match target {
        Target::Ann => {
            let document = documents.ann().ok_or_else(missing_document)?;
            (
                ANN_STORAGE_UID,
                document.sop_instance_uid(),
                document.series_instance_uid(),
            )
        }
        Target::Seg => {
            let document = documents.seg().ok_or_else(missing_document)?;
            (
                SEG_STORAGE_UID,
                document.sop_instance_uid(),
                document.series_instance_uid(),
            )
        }
        Target::Sr => {
            let document = documents.sr().ok_or_else(missing_document)?;
            (
                COMPREHENSIVE_3D_SR_STORAGE_UID,
                document.sop_instance_uid(),
                document.series_instance_uid(),
            )
        }
    };
    OutputReport::for_file(
        target.label(),
        staged_path,
        report_path,
        sop_class_uid,
        sop_instance_uid,
        series_instance_uid,
    )
}

fn pathology_write_error(error: PathologyDocumentWriteError) -> ConversionError {
    match error {
        PathologyDocumentWriteError::Write { source, .. } => ConversionError::output_write(source),
        PathologyDocumentWriteError::Verification { source, .. } => {
            ConversionError::new("OUTPUT_VERIFICATION_FAILED", source.to_string())
        }
        PathologyDocumentWriteError::Mismatch(_) => {
            ConversionError::new("OUTPUT_VERIFICATION_FAILED", error.to_string())
        }
        PathologyDocumentWriteError::MissingTarget => ConversionError::new(
            "INTERNAL_TARGET_MISMATCH",
            "selected target document is missing",
        ),
    }
}

fn base_report(
    arguments: &Arguments,
    annotations: &PathologyAnnotationSet,
    mapping: &[u8],
    geojson: &[u8],
    started: Instant,
) -> Result<SuccessReport, ConversionError> {
    let mut report = SuccessReport::new("convert-geojson");
    report.inputs = vec![
        InputReport::from_file("source", &arguments.source)?,
        InputReport::from_file("canonical_source", &arguments.canonical_source)?,
        InputReport::from_bytes("mapping", &arguments.mapping, mapping)?,
        InputReport::from_bytes("geojson", &arguments.input, geojson)?,
    ];
    report.target_coverage = arguments
        .targets
        .iter()
        .map(|target| CoverageReport::features(target.label(), annotations.feature_count()))
        .collect();
    report.add_diagnostics(annotations.diagnostics());
    report.semantic_digest = annotations.semantic_sha256();
    report
        .timing_ms
        .insert("conversion", started.elapsed().as_secs_f64() * 1_000.0);
    Ok(report)
}

fn missing_document() -> ConversionError {
    ConversionError::new(
        "INTERNAL_TARGET_MISMATCH",
        "selected target document is missing",
    )
}

#[cfg(test)]
mod tests {
    use dicom_dictionary_std::tags;

    use super::*;
    use crate::legacy::tests::write_source_wsi;

    #[test]
    fn output_shape_tracks_target_count() {
        let single = parse(
            [
                "--source",
                "source.dcm",
                "--mapping",
                "mapping.json",
                "--coordinate-space",
                "level0-pixels",
                "--target",
                "ann",
                "--output",
                "ann.dcm",
                "annotations.geojson",
            ]
            .map(OsString::from),
        )
        .unwrap();
        assert_eq!(single.targets, vec![Target::Ann]);
        assert_eq!(single.output, Some(PathBuf::from("ann.dcm")));

        let error = parse(
            [
                "--source",
                "source.dcm",
                "--mapping",
                "mapping.json",
                "--coordinate-space",
                "level0-pixels",
                "--target",
                "ann",
                "--target",
                "seg",
                "--output",
                "one.dcm",
                "annotations.geojson",
            ]
            .map(OsString::from),
        )
        .unwrap_err();
        assert_eq!(error, "multiple targets require --output-dir, not --output");
    }

    #[test]
    fn rejects_repeated_valued_options() {
        let error = parse(
            [
                "--source",
                "source-a.dcm",
                "--source",
                "source-b.dcm",
                "--mapping",
                "mapping.json",
                "--coordinate-space",
                "level0-pixels",
                "--target",
                "ann",
                "--output",
                "ann.dcm",
                "annotations.geojson",
            ]
            .map(OsString::from),
        )
        .unwrap_err();

        assert_eq!(error, "--source may be supplied only once");
    }

    #[test]
    fn publishes_verified_three_object_bundle_and_matching_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.dcm");
        let mapping = directory.path().join("mapping.json");
        let geojson = directory.path().join("annotations.geojson");
        let output = directory.path().join("bundle");
        write_source_wsi(&source);
        std::fs::write(&mapping, MAPPING).unwrap();
        std::fs::write(&geojson, GEOJSON).unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit = execute(
            [
                "--source",
                source.to_str().unwrap(),
                "--mapping",
                mapping.to_str().unwrap(),
                "--coordinate-space",
                "level0-pixels",
                "--target",
                "ann",
                "--target",
                "seg",
                "--target",
                "sr",
                "--output-dir",
                output.to_str().unwrap(),
                geojson.to_str().unwrap(),
            ]
            .map(OsString::from),
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit, 0, "{}", String::from_utf8_lossy(&stderr));
        assert!(stderr.is_empty());
        for name in ["ann.dcm", "seg.dcm", "sr.dcm", "manifest.json"] {
            assert!(output.join(name).is_file(), "missing {name}");
        }
        for (name, series_number) in [("ann.dcm", "9101"), ("seg.dcm", "9201"), ("sr.dcm", "9301")]
        {
            let object = dicom_object::open_file(output.join(name)).unwrap();
            assert_eq!(
                object
                    .element(tags::MANUFACTURER)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "Frames"
            );
            assert_eq!(
                object
                    .element(tags::MANUFACTURER_MODEL_NAME)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "DICOM Viewer"
            );
            assert_eq!(
                object
                    .element(tags::SERIES_NUMBER)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                series_number
            );
        }
        let report: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(report, manifest);
        assert_eq!(report["status"], "ok");
        assert_eq!(report["semantic_digest"].as_str().unwrap().len(), 64);
        assert_eq!(
            report["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|output| output["target"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["ann", "seg", "sr"]
        );
    }

    #[test]
    fn conversion_failure_does_not_publish_a_partial_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.dcm");
        let mapping = directory.path().join("mapping.json");
        let geojson = directory.path().join("point.geojson");
        let output = directory.path().join("bundle");
        write_source_wsi(&source);
        std::fs::write(&mapping, MAPPING).unwrap();
        std::fs::write(
            &geojson,
            br#"{"type":"FeatureCollection","features":[{"type":"Feature","id":"2.25.70","geometry":{"type":"Point","coordinates":[1,1]},"properties":{"classification":{"name":"tumor"}}}]}"#,
        )
        .unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit = execute(
            [
                "--source",
                source.to_str().unwrap(),
                "--mapping",
                mapping.to_str().unwrap(),
                "--coordinate-space",
                "level0-pixels",
                "--target",
                "ann",
                "--target",
                "seg",
                "--output-dir",
                output.to_str().unwrap(),
                geojson.to_str().unwrap(),
            ]
            .map(OsString::from),
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit, 1);
        assert!(!output.exists());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&stdout).unwrap()["error"]["code"],
            "CONVERSION_FAILED"
        );
    }

    const MAPPING: &[u8] = br#"
    {
      "schema_version":1,
      "labels":{
        "tumor":{
          "category":{"code_value":"M-01000","coding_scheme_designator":"SRT","code_meaning":"Morphologically Altered Structure"},
          "property_type":{"code_value":"108369006","coding_scheme_designator":"SCT","code_meaning":"Neoplasm"},
          "generation_type":"MANUAL",
          "recommended_display_cielab":[40000,30000,20000],
          "segment_label":"Tumor"
        }
      },
      "measurements":{
        "Area":{
          "concept":{"code_value":"AREA","coding_scheme_designator":"99WSI","code_meaning":"Area"},
          "unit":{"code_value":"mm2","coding_scheme_designator":"UCUM","code_meaning":"square millimeter"}
        }
      },
      "sr":{
        "report_title":{"code_value":"126000","coding_scheme_designator":"DCM","code_meaning":"Imaging Measurement Report"},
        "procedures_reported":[{"code_value":"P5-09051","coding_scheme_designator":"SRT","code_meaning":"Histopathology procedure"}]
      }
    }
    "#;

    const GEOJSON: &[u8] = br#"
    {"type":"FeatureCollection","features":[{
      "type":"Feature",
      "id":"2.25.71",
      "geometry":{"type":"Polygon","coordinates":[[[1,1],[6,1],[6,6],[1,6],[1,1]]]},
      "properties":{"classification":{"name":"tumor"},"measurements":{"Area":25.0}}
    }]}
    "#;
}
