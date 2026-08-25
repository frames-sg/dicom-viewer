use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use dicom_viewer_core::frames_viewer_producer;
use wsi_dicom_annotations::{
    DicomAnnotationContext, DicomBundlePublication, DicomSinglePublication, ParametricMapDocument,
    ParametricMapInstance, ParametricMapPlan, RasterChannelSelection, RasterProfile,
};

use crate::command::{next_path, next_utf8, set_once, USAGE};
use crate::conversion_report::{
    emit_error, emit_success, emit_usage_error, write_manifest, ConversionError, CoverageReport,
    InputReport, OutputReport, SuccessReport,
};
use crate::publication::{publication_error, read_bounded_file};
use crate::PEAK_ALLOC;

pub(crate) const DEFAULT_MAX_INSTANCE_BYTES: u64 = 2_000_000_000;
const MAX_PROFILE_BYTES: u64 = 4 * 1024 * 1024;
const PARAMETRIC_MAP_STORAGE_UID: &str = "1.2.840.10008.5.1.4.1.1.30";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChannelSelection {
    One(String),
    All,
}

#[derive(Debug)]
pub(crate) struct Arguments {
    pub(crate) source: PathBuf,
    pub(crate) canonical_source: PathBuf,
    pub(crate) profile: PathBuf,
    pub(crate) channel: Option<ChannelSelection>,
    pub(crate) output: Option<PathBuf>,
    pub(crate) output_dir: Option<PathBuf>,
    pub(crate) max_instance_bytes: u64,
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
            return emit_usage_error("convert-raster", &error, USAGE, stdout, stderr);
        }
    };
    match run(&arguments) {
        Ok(report) => emit_success(&report, stdout),
        Err(error) => emit_error("convert-raster", &error, stdout, stderr),
    }
}

pub(crate) fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let mut arguments = arguments.into_iter();
    let mut source = None;
    let mut canonical_source = None;
    let mut profile = None;
    let mut channel = None;
    let mut output = None;
    let mut output_dir = None;
    let mut max_instance_bytes = None;
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
            Some("--profile") => {
                let value = next_path(&mut arguments, "--profile")?;
                set_once(&mut profile, value, "--profile")?;
            }
            Some("--channel") => {
                if channel.is_some() {
                    return Err("--channel and --all-channels are mutually exclusive".into());
                }
                channel = Some(ChannelSelection::One(next_utf8(
                    &mut arguments,
                    "--channel",
                )?));
            }
            Some("--all-channels") => {
                if channel.is_some() {
                    return Err("--channel and --all-channels are mutually exclusive".into());
                }
                channel = Some(ChannelSelection::All);
            }
            Some("--output") => {
                let value = next_path(&mut arguments, "--output")?;
                set_once(&mut output, value, "--output")?;
            }
            Some("--output-dir") => {
                let value = next_path(&mut arguments, "--output-dir")?;
                set_once(&mut output_dir, value, "--output-dir")?;
            }
            Some("--max-instance-bytes") => {
                let value = next_utf8(&mut arguments, "--max-instance-bytes")?;
                let parsed = value.parse().map_err(|_| {
                    "--max-instance-bytes requires a positive integer byte count".to_string()
                })?;
                if parsed == 0 {
                    return Err(
                        "--max-instance-bytes requires a positive integer byte count".into(),
                    );
                }
                set_once(&mut max_instance_bytes, parsed, "--max-instance-bytes")?;
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option {value}"));
            }
            _ if input.is_none() => input = Some(PathBuf::from(argument)),
            _ => return Err("only one raster input path may be supplied".into()),
        }
    }
    let source = source.ok_or_else(|| "--source is required".to_string())?;
    let profile = profile.ok_or_else(|| "--profile is required".to_string())?;
    match (&output, &output_dir) {
        (Some(_), None) | (None, Some(_)) => {}
        (None, None) => return Err("--output or --output-dir is required".into()),
        (Some(_), Some(_)) => {
            return Err("--output and --output-dir are mutually exclusive".into());
        }
    }
    let input = input.ok_or_else(|| "a raster input path is required".to_string())?;
    Ok(Arguments {
        canonical_source: canonical_source.unwrap_or_else(|| source.clone()),
        source,
        profile,
        channel,
        output,
        output_dir,
        max_instance_bytes: max_instance_bytes.unwrap_or(DEFAULT_MAX_INSTANCE_BYTES),
        input,
    })
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
    let profile_bytes = read_bounded_file(&arguments.profile, MAX_PROFILE_BYTES, "raster profile")?;
    let profile = RasterProfile::from_json(&profile_bytes).map_err(ConversionError::conversion)?;
    let channel_selection = resolve_channel_selection(&profile, arguments.channel.as_ref())?;
    let document = ParametricMapDocument::open(
        source,
        canonical_source,
        profile,
        &arguments.input,
        channel_selection,
    )
    .map_err(ConversionError::conversion)?
    .with_producer(
        frames_viewer_producer(9401, "WSI parametric maps").map_err(ConversionError::conversion)?,
    );
    let plan = document
        .plan(arguments.max_instance_bytes)
        .map_err(ConversionError::conversion)?;
    let mut report = base_report(arguments, &document, &profile_bytes, started)?;
    let protected = [
        arguments.source.as_path(),
        arguments.canonical_source.as_path(),
        arguments.profile.as_path(),
        arguments.input.as_path(),
    ];
    if let Some(output) = &arguments.output {
        write_single_output(&document, &plan, output, &protected, &mut report, started)?;
    } else if let Some(output_dir) = &arguments.output_dir {
        write_bundle_output(
            &document,
            &plan,
            output_dir,
            &protected,
            &mut report,
            started,
        )?;
    } else {
        return Err(ConversionError::new(
            "INVALID_DESTINATION",
            "conversion has no destination",
        ));
    }
    Ok(report)
}

fn resolve_channel_selection(
    profile: &RasterProfile,
    selection: Option<&ChannelSelection>,
) -> Result<RasterChannelSelection, ConversionError> {
    match selection {
        None => Ok(RasterChannelSelection::Auto),
        Some(ChannelSelection::All) => Ok(RasterChannelSelection::All),
        Some(ChannelSelection::One(value)) => {
            if (0..profile.channel_count())
                .any(|index| profile.channel_name(index) == Some(value.as_str()))
            {
                return Ok(RasterChannelSelection::Name(value.clone()));
            }
            value
                .parse::<usize>()
                .map(RasterChannelSelection::Index)
                .map_err(|_| {
                    ConversionError::new(
                        "CHANNEL_INVALID",
                        format!("channel {value:?} is neither an exact declared name nor an index"),
                    )
                })
        }
    }
}

fn write_single_output(
    document: &ParametricMapDocument,
    plan: &ParametricMapPlan,
    output: &std::path::Path,
    protected: &[&std::path::Path],
    report: &mut SuccessReport,
    started: Instant,
) -> Result<(), ConversionError> {
    if plan.parts().len() != 1 {
        return Err(ConversionError::new(
            "OUTPUT_REQUIRES_DIRECTORY",
            format!(
                "Parametric Map requires {} concatenation parts; use --output-dir",
                plan.parts().len()
            ),
        ));
    }
    let publication =
        DicomSinglePublication::new(output, "pm.dcm", protected).map_err(publication_error)?;
    let paths = [publication.staged_file()];
    let instances = document
        .write_planned_parts(plan, &paths)
        .map_err(ConversionError::output_write)?;
    let instance = instances.first().ok_or_else(|| {
        ConversionError::new(
            "OUTPUT_WRITE_FAILED",
            "single-instance PM write returned no instance",
        )
    })?;
    report.outputs.push(pm_output_report(
        publication.staged_file(),
        publication.destination(),
        instance,
        plan,
        0,
    )?);
    report.record_verification_completion(started, PEAK_ALLOC.peak_usage());
    publication.publish().map_err(publication_error)?;
    Ok(())
}

fn write_bundle_output(
    document: &ParametricMapDocument,
    plan: &ParametricMapPlan,
    output_dir: &std::path::Path,
    protected: &[&std::path::Path],
    report: &mut SuccessReport,
    started: Instant,
) -> Result<(), ConversionError> {
    let publication =
        DicomBundlePublication::new(output_dir, protected).map_err(publication_error)?;
    let names = (1..=plan.parts().len())
        .map(|number| format!("pm-{number:04}.dcm"))
        .collect::<Vec<_>>();
    let staged_paths = names
        .iter()
        .map(|name| publication.staging_path().join(name))
        .collect::<Vec<_>>();
    let instances = document
        .write_planned_parts(plan, &staged_paths)
        .map_err(ConversionError::output_write)?;
    if instances.len() != plan.parts().len() {
        return Err(ConversionError::new(
            "OUTPUT_WRITE_FAILED",
            "PM writer returned a different number of instances than planned",
        ));
    }
    for (index, ((name, staged), instance)) in
        names.iter().zip(&staged_paths).zip(&instances).enumerate()
    {
        report.outputs.push(pm_output_report(
            staged,
            &publication.destination().join(name),
            instance,
            plan,
            index,
        )?);
    }
    report.record_verification_completion(started, PEAK_ALLOC.peak_usage());
    let manifest = publication.staging_path().join("manifest.json");
    write_manifest(&manifest, report)?;
    publication
        .sync_staged_file(&manifest)
        .map_err(publication_error)?;
    publication.publish().map_err(publication_error)?;
    Ok(())
}

fn base_report(
    arguments: &Arguments,
    document: &ParametricMapDocument,
    profile_bytes: &[u8],
    started: Instant,
) -> Result<SuccessReport, ConversionError> {
    let mut report = SuccessReport::new("convert-raster");
    report.inputs = vec![
        InputReport::from_file("source", &arguments.source)?,
        InputReport::from_file("canonical_source", &arguments.canonical_source)?,
        InputReport::from_bytes("raster_profile", &arguments.profile, profile_bytes)?,
        InputReport::from_path("raster", &arguments.input)?,
    ];
    report.target_coverage = vec![CoverageReport::raster(
        document.frame_count(),
        document.selected_channel_count(),
    )];
    report.semantic_digest = document.semantic_digest().to_string();
    report.add_diagnostics(document.diagnostics());
    report
        .timing_ms
        .insert("conversion", started.elapsed().as_secs_f64() * 1_000.0);
    Ok(report)
}

fn pm_output_report(
    staged_path: &std::path::Path,
    report_path: &std::path::Path,
    instance: &ParametricMapInstance,
    plan: &ParametricMapPlan,
    part_index: usize,
) -> Result<OutputReport, ConversionError> {
    let part = plan.parts().get(part_index).ok_or_else(|| {
        ConversionError::new(
            "OUTPUT_REPORT_FAILED",
            "PM output has no matching planned part",
        )
    })?;
    OutputReport::for_file(
        "pm",
        staged_path,
        report_path,
        PARAMETRIC_MAP_STORAGE_UID,
        instance.sop_instance_uid(),
        instance.series_instance_uid(),
    )
    .and_then(|report| report.with_parametric_map_part(plan, part, part_index))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write as _;

    use dicom_dictionary_std::{tags, uids};

    use super::*;
    use crate::legacy::tests::write_source_wsi;

    #[test]
    fn defaults_instance_limit_and_rejects_conflicting_channels() {
        let parsed = parse(
            [
                "--source",
                "source.dcm",
                "--profile",
                "profile.json",
                "--output",
                "map.dcm",
                "map.npy",
            ]
            .map(OsString::from),
        )
        .unwrap();
        assert_eq!(parsed.max_instance_bytes, DEFAULT_MAX_INSTANCE_BYTES);
        assert_eq!(parsed.channel, None);

        let error = parse(
            [
                "--source",
                "source.dcm",
                "--profile",
                "profile.json",
                "--channel",
                "0",
                "--all-channels",
                "--output",
                "map.dcm",
                "map.npy",
            ]
            .map(OsString::from),
        )
        .unwrap_err();
        assert_eq!(error, "--channel and --all-channels are mutually exclusive");
    }

    #[test]
    fn rejects_repeated_valued_options() {
        let error = parse(
            [
                "--source",
                "source.dcm",
                "--profile",
                "profile-a.json",
                "--profile",
                "profile-b.json",
                "--output",
                "map.dcm",
                "map.npy",
            ]
            .map(OsString::from),
        )
        .unwrap_err();

        assert_eq!(error, "--profile may be supplied only once");
    }

    #[test]
    fn converts_single_channel_npy_and_emits_one_success_report() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.dcm");
        let profile = directory.path().join("profile.json");
        let input = directory.path().join("probability.npy");
        let output = directory.path().join("map.dcm");
        write_source_wsi(&source);
        std::fs::write(&profile, profile_json(false)).unwrap();
        write_npy(&input, &[2, 2], &[0.0, 0.25, f32::NAN, 1.0]);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit = execute(
            [
                "--source",
                source.to_str().unwrap(),
                "--profile",
                profile.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                input.to_str().unwrap(),
            ]
            .map(OsString::from),
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit, 0, "{}", String::from_utf8_lossy(&stderr));
        let report: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(report["schema"], "conversion-report-v1");
        assert_eq!(report["operation"], "convert-raster");
        let published_output = output.canonicalize().unwrap();
        assert_eq!(
            report["outputs"][0]["path"],
            published_output.to_string_lossy().as_ref()
        );
        assert_eq!(report["target_coverage"][0]["frame_count"], 1);
        assert_eq!(
            report["normalizations"][0]["code"],
            "RASTER_NAN_CANONICALIZED"
        );
        let object = dicom_object::open_file(&output).unwrap();
        assert_eq!(
            object.meta().media_storage_sop_class_uid(),
            uids::PARAMETRIC_MAP_STORAGE
        );
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
    }

    #[test]
    fn publishes_deterministically_named_all_channel_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.dcm");
        let profile = directory.path().join("profile.json");
        let input = directory.path().join("probability.npy");
        let output = directory.path().join("maps");
        write_source_wsi(&source);
        std::fs::write(&profile, profile_json(true)).unwrap();
        write_npy(
            &input,
            &[2, 2, 2],
            &[0.0, 1.0, 0.25, 0.75, 0.5, 0.5, 1.0, 0.0],
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit = execute(
            [
                "--source",
                source.to_str().unwrap(),
                "--profile",
                profile.to_str().unwrap(),
                "--all-channels",
                "--output-dir",
                output.to_str().unwrap(),
                input.to_str().unwrap(),
            ]
            .map(OsString::from),
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit, 0, "{}", String::from_utf8_lossy(&stderr));
        assert!(output.join("pm-0001.dcm").is_file());
        assert!(output.join("manifest.json").is_file());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["target_coverage"][0]["channel_count"], 2);
        assert_eq!(manifest["target_coverage"][0]["frame_count"], 2);
        assert_eq!(manifest["outputs"].as_array().unwrap().len(), 1);
        let published_output = output.canonicalize().unwrap().join("pm-0001.dcm");
        assert_eq!(
            manifest["outputs"][0]["path"],
            published_output.to_string_lossy().as_ref()
        );
    }

    fn write_npy(path: &std::path::Path, shape: &[usize], values: &[f32]) {
        let shape = shape
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let mut header =
            format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({shape},), }}");
        while (10 + header.len() + 1) % 64 != 0 {
            header.push(' ');
        }
        header.push('\n');
        let mut file = File::create(path).unwrap();
        file.write_all(b"\x93NUMPY\x01\x00").unwrap();
        file.write_all(&u16::try_from(header.len()).unwrap().to_le_bytes())
            .unwrap();
        file.write_all(header.as_bytes()).unwrap();
        for value in values {
            file.write_all(&value.to_le_bytes()).unwrap();
        }
    }

    fn profile_json(multichannel: bool) -> String {
        let axes = if multichannel {
            r#"["y","x","channel"]"#
        } else {
            r#"["y","x"]"#
        };
        let second = if multichannel {
            r#",{"name":"stroma","quantity":{"code_value":"STROMA","coding_scheme_designator":"99WSI","code_meaning":"Stroma probability"},"unit":{"code_value":"1","coding_scheme_designator":"UCUM","code_meaning":"no units"}}"#
        } else {
            ""
        };
        format!(
            r#"{{
              "schema_version":1,
              "input_format":"npy",
              "dtype":"float32",
              "axes":{axes},
              "grid_origin":{{"x":0.0,"y":0.0}},
              "sample_spacing":{{"x":1.0,"y":1.0}},
              "coordinate_space":"level0-pixels",
              "channels":[{{"name":"tumor","quantity":{{"code_value":"TUMOR","coding_scheme_designator":"99WSI","code_meaning":"Tumor probability"}},"unit":{{"code_value":"1","coding_scheme_designator":"UCUM","code_meaning":"no units"}}}}{second}],
              "algorithm":{{"family":{{"code_value":"123110","coding_scheme_designator":"DCM","code_meaning":"Artificial Intelligence"}},"name":"Example model","version":"1.0"}}
            }}"#
        )
    }
}
