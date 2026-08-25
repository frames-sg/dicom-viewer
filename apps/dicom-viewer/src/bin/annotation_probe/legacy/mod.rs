#![forbid(unsafe_code)]

mod report;
mod schema;

#[cfg(test)]
pub(crate) mod tests;

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use wsi_dicom_annotations::{
    annotation_object_kind, AnnotationDocument, AnnotationObjectKind, DicomAnnotationContext,
    Error as AnnotationError, SegmentationDocument,
};

use crate::command::next_path;
use crate::conversion_report::write_json_line;
use crate::PEAK_ALLOC;

use self::report::{
    build_ann_report, build_seg_report, diagnostic_reports, file_report, operation_error_report,
    success_report,
};
use self::schema::{SemanticReport, SuccessReport};

const SCHEMA_VERSION: u32 = 1;
const USAGE: &str = "usage:\n  annotation_probe inspect --source <referenced-wsi> [--canonical-source <level0-wsi>] [--payload full|digest] <ann-or-seg>\n  annotation_probe roundtrip --source <referenced-wsi> [--canonical-source <level0-wsi>] --output <new.dcm> [--allow-lossy] [--payload full|digest] <ann-or-seg>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Inspect,
    Roundtrip,
}

impl Operation {
    const fn label(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Roundtrip => "roundtrip",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayloadMode {
    Full,
    Digest,
}

impl PayloadMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Digest => "digest",
        }
    }
}

#[derive(Debug)]
struct Arguments {
    operation: Operation,
    source: PathBuf,
    canonical_source: PathBuf,
    output: Option<PathBuf>,
    allow_lossy: bool,
    payload: PayloadMode,
    input: PathBuf,
}

#[derive(Debug)]
struct ProbeError {
    code: &'static str,
    message: String,
}

impl ProbeError {
    fn rewrite(object_kind: &str, error: AnnotationError) -> Self {
        let code = if matches!(error, AnnotationError::Unsupported(_)) {
            "REWRITE_UNSUPPORTED"
        } else {
            "OPERATION_FAILED"
        };
        Self {
            code,
            message: format!("{object_kind} rewrite failed: {error}"),
        }
    }
}

impl From<String> for ProbeError {
    fn from(message: String) -> Self {
        Self {
            code: "OPERATION_FAILED",
            message,
        }
    }
}

pub(crate) fn execute(
    arguments: impl IntoIterator<Item = OsString>,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> i32 {
    let arguments = match parse_arguments(arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            let _ = writeln!(stderr, "{error}\n{USAGE}");
            return 2;
        }
    };
    PEAK_ALLOC.reset_peak_usage();
    match run_probe(&arguments) {
        Ok(report) => {
            for diagnostic in &report.diagnostics {
                let _ = writeln!(
                    stderr,
                    "[{}] {} {}: {}",
                    diagnostic.severity, diagnostic.code, diagnostic.path, diagnostic.message
                );
            }
            if write_json_line(&mut stdout, &report).is_ok() {
                0
            } else {
                let _ = writeln!(stderr, "failed to serialize probe report");
                1
            }
        }
        Err(error) => {
            let report = operation_error_report(&arguments, error.code, error.message.clone());
            let _ = write_json_line(&mut stdout, &report);
            let _ = writeln!(stderr, "annotation probe failed: {}", error.message);
            1
        }
    }
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let mut arguments = arguments.into_iter();
    let operation = match arguments.next().as_deref().and_then(|value| value.to_str()) {
        Some("inspect") => Operation::Inspect,
        Some("roundtrip") => Operation::Roundtrip,
        _ => return Err("the first argument must be inspect or roundtrip".into()),
    };
    let mut source = None;
    let mut canonical_source = None;
    let mut output = None;
    let mut allow_lossy = false;
    let mut payload = PayloadMode::Full;
    let mut input = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--source") => {
                source = Some(next_path(&mut arguments, "--source")?);
            }
            Some("--canonical-source") => {
                canonical_source = Some(next_path(&mut arguments, "--canonical-source")?);
            }
            Some("--output") => {
                output = Some(next_path(&mut arguments, "--output")?);
            }
            Some("--allow-lossy") => allow_lossy = true,
            Some("--payload") => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--payload requires full or digest".to_string())?;
                payload = match value.to_str() {
                    Some("full") => PayloadMode::Full,
                    Some("digest") => PayloadMode::Digest,
                    _ => return Err("--payload requires full or digest".into()),
                };
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option {value}"));
            }
            _ if input.is_none() => input = Some(PathBuf::from(argument)),
            _ => return Err("only one ANN or SEG input path may be supplied".into()),
        }
    }
    let source = source.ok_or_else(|| "--source is required".to_string())?;
    let input = input.ok_or_else(|| "an ANN or SEG input path is required".to_string())?;
    match operation {
        Operation::Inspect if output.is_some() || allow_lossy => {
            return Err("--output and --allow-lossy are only valid for roundtrip".into());
        }
        Operation::Roundtrip if output.is_none() => {
            return Err("roundtrip requires --output".into());
        }
        _ => {}
    }
    if output.as_ref().is_some_and(|path| path == &input) {
        return Err("roundtrip output must differ from the input object".into());
    }
    Ok(Arguments {
        operation,
        canonical_source: canonical_source.unwrap_or_else(|| source.clone()),
        source,
        output,
        allow_lossy,
        payload,
        input,
    })
}

fn run_probe(arguments: &Arguments) -> Result<SuccessReport, ProbeError> {
    let input_bytes = file_size(&arguments.input)?;
    let parse_started = Instant::now();
    let source = DicomAnnotationContext::from_source(&arguments.source)
        .map_err(|error| format!("source WSI could not be read: {error}"))?;
    let canonical_source = DicomAnnotationContext::from_source(&arguments.canonical_source)
        .map_err(|error| format!("canonical source WSI could not be read: {error}"))?;
    let kind = annotation_object_kind(&arguments.input)
        .map_err(|error| format!("input type could not be identified: {error}"))?;

    match kind {
        AnnotationObjectKind::Annotation => run_ann(
            arguments,
            &source,
            &canonical_source,
            input_bytes,
            parse_started,
        ),
        AnnotationObjectKind::Segmentation => run_seg(
            arguments,
            &source,
            &canonical_source,
            input_bytes,
            parse_started,
        ),
    }
}

fn run_ann(
    arguments: &Arguments,
    source: &DicomAnnotationContext,
    canonical_source: &DicomAnnotationContext,
    input_bytes: u64,
    parse_started: Instant,
) -> Result<SuccessReport, ProbeError> {
    let input_document = AnnotationDocument::read_ann(&arguments.input, source)
        .map_err(|error| format!("ANN parse failed: {error}"))?;
    let parse_ms = elapsed_ms(parse_started.elapsed());
    let input_file = file_report(
        &arguments.input,
        input_bytes,
        input_document.sop_instance_uid(),
        input_document.series_instance_uid(),
    );
    let input_diagnostics = diagnostic_reports(input_document.diagnostics());
    let (document, output, write_ms, verify_ms) = match arguments.operation {
        Operation::Inspect => (input_document, None, 0.0, 0.0),
        Operation::Roundtrip => {
            let Some(output_path) = arguments.output.as_ref() else {
                return Err("roundtrip output path was not validated".to_owned().into());
            };
            reject_source_aliases(arguments, output_path)?;
            let revised = input_document.revised();
            let write_started = Instant::now();
            revised
                .write_ann_with_loss_policy(output_path, arguments.allow_lossy)
                .map_err(|error| ProbeError::rewrite("ANN", error))?;
            let write_ms = elapsed_ms(write_started.elapsed());
            let verify_started = Instant::now();
            let output_document = AnnotationDocument::read_ann(output_path, source)
                .map_err(|error| format!("rewritten ANN verification failed: {error}"))?;
            let verify_ms = elapsed_ms(verify_started.elapsed());
            let output = Some(file_report(
                output_path,
                file_size(output_path)?,
                output_document.sop_instance_uid(),
                output_document.series_instance_uid(),
            ));
            (output_document, output, write_ms, verify_ms)
        }
    };
    let canonicalize_started = Instant::now();
    let semantic = SemanticReport::Ann(build_ann_report(
        &document,
        canonical_source,
        arguments.payload,
    )?);
    Ok(success_report(
        arguments,
        input_file,
        output,
        semantic,
        input_diagnostics,
        parse_ms,
        write_ms,
        verify_ms,
        elapsed_ms(canonicalize_started.elapsed()),
    ))
}

fn run_seg(
    arguments: &Arguments,
    source: &DicomAnnotationContext,
    canonical_source: &DicomAnnotationContext,
    input_bytes: u64,
    parse_started: Instant,
) -> Result<SuccessReport, ProbeError> {
    let input_document = SegmentationDocument::read_seg(&arguments.input, source)
        .map_err(|error| format!("SEG parse failed: {error}"))?;
    let parse_ms = elapsed_ms(parse_started.elapsed());
    let input_file = file_report(
        &arguments.input,
        input_bytes,
        input_document.sop_instance_uid(),
        input_document.series_instance_uid(),
    );
    let input_diagnostics = diagnostic_reports(input_document.diagnostics());
    let (document, output, write_ms, verify_ms) = match arguments.operation {
        Operation::Inspect => (input_document, None, 0.0, 0.0),
        Operation::Roundtrip => {
            let Some(output_path) = arguments.output.as_ref() else {
                return Err("roundtrip output path was not validated".to_owned().into());
            };
            reject_source_aliases(arguments, output_path)?;
            let revised = input_document.revised();
            let write_started = Instant::now();
            revised
                .write_seg_with_loss_policy(output_path, arguments.allow_lossy)
                .map_err(|error| ProbeError::rewrite("SEG", error))?;
            let write_ms = elapsed_ms(write_started.elapsed());
            let verify_started = Instant::now();
            let output_document = SegmentationDocument::read_seg(output_path, source)
                .map_err(|error| format!("rewritten SEG verification failed: {error}"))?;
            let verify_ms = elapsed_ms(verify_started.elapsed());
            let output = Some(file_report(
                output_path,
                file_size(output_path)?,
                output_document.sop_instance_uid(),
                output_document.series_instance_uid(),
            ));
            (output_document, output, write_ms, verify_ms)
        }
    };
    let canonicalize_started = Instant::now();
    let semantic = SemanticReport::Seg(build_seg_report(
        &document,
        canonical_source,
        arguments.payload,
    )?);
    Ok(success_report(
        arguments,
        input_file,
        output,
        semantic,
        input_diagnostics,
        parse_ms,
        write_ms,
        verify_ms,
        elapsed_ms(canonicalize_started.elapsed()),
    ))
}

fn reject_source_aliases(arguments: &Arguments, output: &Path) -> Result<(), String> {
    for (name, path) in [
        ("source", &arguments.source),
        ("canonical source", &arguments.canonical_source),
    ] {
        if output == path
            || output
                .canonicalize()
                .ok()
                .zip(path.canonicalize().ok())
                .is_some_and(|(output, source)| output == source)
        {
            return Err(format!("roundtrip output cannot replace the {name} WSI"));
        }
    }
    Ok(())
}

fn file_size(path: &Path) -> Result<u64, String> {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("could not read {} metadata: {error}", path.display()))
}

fn elapsed_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
