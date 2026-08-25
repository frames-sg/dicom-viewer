use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use serde::Serialize;
use wsi_dicom_annotations::{
    DiagnosticDisposition, DiagnosticSeverity, InteroperabilityDiagnostic,
};

#[path = "conversion_report/checksum.rs"]
mod checksum;

use checksum::{sha256_bytes, sha256_directory, sha256_file};

pub(crate) const SCHEMA: &str = "conversion-report-v1";
pub(crate) const SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub(crate) struct ConversionError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl ConversionError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn io(operation: &'static str, path: &Path, error: impl std::fmt::Display) -> Self {
        Self::new(operation, format!("{}: {error}", path.to_string_lossy()))
    }

    pub(crate) fn conversion(error: impl std::fmt::Display) -> Self {
        Self::new("CONVERSION_FAILED", error.to_string())
    }

    pub(crate) fn output_write(error: impl std::fmt::Display) -> Self {
        Self::new("OUTPUT_WRITE_FAILED", error.to_string())
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ImplementationReport {
    name: &'static str,
    version: &'static str,
}

impl Default for ImplementationReport {
    fn default() -> Self {
        Self {
            name: "dicom-viewer-rust",
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct InputReport {
    role: &'static str,
    path: String,
    bytes: u64,
    sha256: String,
}

impl InputReport {
    pub(crate) fn from_bytes(
        role: &'static str,
        path: &Path,
        bytes: &[u8],
    ) -> Result<Self, ConversionError> {
        let bytes_len = u64::try_from(bytes.len()).map_err(|_| {
            ConversionError::new("INPUT_TOO_LARGE", "input byte length does not fit u64")
        })?;
        Ok(Self {
            role,
            path: path.to_string_lossy().into_owned(),
            bytes: bytes_len,
            sha256: sha256_bytes(bytes),
        })
    }

    pub(crate) fn from_file(role: &'static str, path: &Path) -> Result<Self, ConversionError> {
        let (bytes, sha256) = sha256_file(path)?;
        Ok(Self {
            role,
            path: path.to_string_lossy().into_owned(),
            bytes,
            sha256,
        })
    }

    pub(crate) fn from_path(role: &'static str, path: &Path) -> Result<Self, ConversionError> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| ConversionError::io("INPUT_READ_FAILED", path, error))?;
        let (bytes, sha256) = if metadata.is_file() {
            sha256_file(path)?
        } else if metadata.is_dir() {
            sha256_directory(path)?
        } else {
            return Err(ConversionError::new(
                "INPUT_PATH_INVALID",
                format!(
                    "input is neither a regular file nor a directory: {}",
                    path.display()
                ),
            ));
        };
        Ok(Self {
            role,
            path: path.to_string_lossy().into_owned(),
            bytes,
            sha256,
        })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct OutputReport {
    pub(crate) target: &'static str,
    pub(crate) path: String,
    pub(crate) sop_class_uid: &'static str,
    pub(crate) sop_instance_uid: String,
    pub(crate) series_instance_uid: String,
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) concatenation_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) concatenation_source_sop_instance_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) in_concatenation_number: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) in_concatenation_total_number: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frame_offset: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frame_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pixel_value_bytes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pixel_sha256: Option<String>,
}

impl OutputReport {
    pub(crate) fn for_file(
        target: &'static str,
        path: &Path,
        report_path: &Path,
        sop_class_uid: &'static str,
        sop_instance_uid: &str,
        series_instance_uid: &str,
    ) -> Result<Self, ConversionError> {
        let (bytes, sha256) = sha256_file(path)?;
        Ok(Self {
            target,
            path: report_path.to_string_lossy().into_owned(),
            sop_class_uid,
            sop_instance_uid: sop_instance_uid.to_string(),
            series_instance_uid: series_instance_uid.to_string(),
            bytes,
            sha256,
            concatenation_uid: None,
            concatenation_source_sop_instance_uid: None,
            in_concatenation_number: None,
            in_concatenation_total_number: None,
            frame_offset: None,
            frame_count: None,
            pixel_value_bytes: None,
            pixel_sha256: None,
        })
    }

    pub(crate) fn with_parametric_map_part(
        mut self,
        plan: &wsi_dicom_annotations::ParametricMapPlan,
        part: &wsi_dicom_annotations::ParametricMapPartPlan,
        part_index: usize,
    ) -> Result<Self, ConversionError> {
        let concatenated = plan.parts().len() > 1;
        self.concatenation_uid = plan.concatenation_uid().map(str::to_string);
        self.concatenation_source_sop_instance_uid = plan
            .concatenation_source_sop_instance_uid()
            .map(str::to_string);
        if concatenated {
            self.in_concatenation_number = Some(u16::try_from(part_index + 1).map_err(|_| {
                ConversionError::new(
                    "OUTPUT_REPORT_FAILED",
                    "PM concatenation part number exceeds DICOM US",
                )
            })?);
            self.in_concatenation_total_number =
                Some(u16::try_from(plan.parts().len()).map_err(|_| {
                    ConversionError::new(
                        "OUTPUT_REPORT_FAILED",
                        "PM concatenation total exceeds DICOM US",
                    )
                })?);
        }
        self.frame_offset = Some(part.frame_offset());
        self.frame_count = Some(part.frame_count());
        self.pixel_value_bytes = Some(part.pixel_value_length());
        self.pixel_sha256 = Some(part.pixel_sha256().to_string());
        Ok(self)
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum CoverageReport {
    Features {
        target: &'static str,
        feature_count: usize,
    },
    Raster {
        target: &'static str,
        frame_count: u32,
        channel_count: usize,
    },
}

impl CoverageReport {
    pub(crate) const fn features(target: &'static str, feature_count: usize) -> Self {
        Self::Features {
            target,
            feature_count,
        }
    }

    pub(crate) const fn raster(frame_count: u32, channel_count: usize) -> Self {
        Self::Raster {
            target: "pm",
            frame_count,
            channel_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct DiagnosticReport {
    code: String,
    severity: &'static str,
    path: String,
    disposition: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SuccessReport {
    schema: &'static str,
    schema_version: u32,
    status: &'static str,
    operation: &'static str,
    implementation: ImplementationReport,
    pub(crate) inputs: Vec<InputReport>,
    pub(crate) outputs: Vec<OutputReport>,
    pub(crate) target_coverage: Vec<CoverageReport>,
    pub(crate) normalizations: Vec<DiagnosticReport>,
    pub(crate) losses: Vec<DiagnosticReport>,
    pub(crate) semantic_digest: String,
    pub(crate) timing_ms: BTreeMap<&'static str, f64>,
    pub(crate) peak_tracked_heap_bytes: usize,
}

impl SuccessReport {
    pub(crate) fn new(operation: &'static str) -> Self {
        Self {
            schema: SCHEMA,
            schema_version: SCHEMA_VERSION,
            status: "ok",
            operation,
            implementation: ImplementationReport::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            target_coverage: Vec::new(),
            normalizations: Vec::new(),
            losses: Vec::new(),
            semantic_digest: String::new(),
            timing_ms: BTreeMap::new(),
            peak_tracked_heap_bytes: 0,
        }
    }

    pub(crate) fn add_diagnostics(&mut self, diagnostics: &[InteroperabilityDiagnostic]) {
        for diagnostic in diagnostics {
            let report = DiagnosticReport::from(diagnostic);
            match diagnostic.disposition() {
                DiagnosticDisposition::Normalized => self.normalizations.push(report),
                DiagnosticDisposition::WouldDrop => self.losses.push(report),
                DiagnosticDisposition::Unsupported => self.losses.push(report),
            }
        }
    }

    pub(crate) fn record_verification_completion(&mut self, started: Instant, peak_heap: usize) {
        self.peak_tracked_heap_bytes = peak_heap;
        self.timing_ms.insert(
            "through_verification",
            started.elapsed().as_secs_f64() * 1_000.0,
        );
    }
}

impl From<&InteroperabilityDiagnostic> for DiagnosticReport {
    fn from(diagnostic: &InteroperabilityDiagnostic) -> Self {
        Self {
            code: diagnostic.code().to_string(),
            severity: match diagnostic.severity() {
                DiagnosticSeverity::Info => "info",
                DiagnosticSeverity::Warning => "warning",
                DiagnosticSeverity::Error => "error",
            },
            path: diagnostic.path().to_string(),
            disposition: match diagnostic.disposition() {
                DiagnosticDisposition::Normalized => "normalized",
                DiagnosticDisposition::WouldDrop => "would_drop",
                DiagnosticDisposition::Unsupported => "unsupported",
            },
            message: diagnostic.message().to_string(),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'static str,
    message: &'a str,
}

#[derive(Serialize)]
struct ErrorReport<'a> {
    schema: &'static str,
    schema_version: u32,
    status: &'static str,
    operation: &'static str,
    implementation: ImplementationReport,
    error: ErrorBody<'a>,
}

pub(crate) fn emit_success(report: &SuccessReport, mut stdout: impl Write) -> i32 {
    match write_json_line(&mut stdout, report) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

pub(crate) fn emit_error(
    operation: &'static str,
    error: &ConversionError,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> i32 {
    if write_error_report(operation, error, &mut stdout).is_err() {
        let _ = writeln!(stderr, "failed to serialize conversion report");
        return 1;
    }
    let _ = writeln!(stderr, "{operation} failed: {}", error.message);
    1
}

pub(crate) fn emit_usage_error(
    operation: &'static str,
    message: &str,
    usage: &str,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> i32 {
    let error = ConversionError::new("USAGE_ERROR", message);
    if write_error_report(operation, &error, &mut stdout).is_err() {
        let _ = writeln!(stderr, "failed to serialize conversion report");
        return 1;
    }
    let _ = writeln!(stderr, "{message}\n{usage}");
    2
}

fn write_error_report(
    operation: &'static str,
    error: &ConversionError,
    output: &mut impl Write,
) -> std::io::Result<()> {
    let report = ErrorReport {
        schema: SCHEMA,
        schema_version: SCHEMA_VERSION,
        status: "error",
        operation,
        implementation: ImplementationReport::default(),
        error: ErrorBody {
            code: error.code,
            message: &error.message,
        },
    };
    write_json_line(output, &report)
}

pub(crate) fn write_manifest(path: &Path, report: &SuccessReport) -> Result<(), ConversionError> {
    let file = File::create(path)
        .map_err(|error| ConversionError::io("MANIFEST_WRITE_FAILED", path, error))?;
    serde_json::to_writer_pretty(file, report).map_err(|error| {
        ConversionError::new(
            "MANIFEST_WRITE_FAILED",
            format!("{}: {error}", path.display()),
        )
    })
}

pub(crate) fn write_json_line(
    output: &mut impl Write,
    value: &impl Serialize,
) -> std::io::Result<()> {
    serde_json::to_writer(&mut *output, value).map_err(std::io::Error::other)?;
    writeln!(output)
}

#[cfg(test)]
mod tests {
    use std::io;

    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn input_report_hashes_the_exact_consumed_bytes() {
        let bytes = b"profile bytes already read by the converter";

        let report = InputReport::from_bytes("mapping", Path::new("mapping.json"), bytes).unwrap();
        let value = serde_json::to_value(report).unwrap();

        assert_eq!(value["bytes"], bytes.len());
        assert_eq!(value["sha256"], format!("{:x}", Sha256::digest(bytes)));
    }

    #[test]
    fn file_and_directory_input_reports_hash_stable_consumed_content() {
        let temporary = tempfile::tempdir().expect("temporary directory should be created");
        let root = temporary.path().join("array.zarr");
        std::fs::create_dir(&root).expect("input directory should be created");
        std::fs::write(root.join("z.json"), b"metadata").expect("metadata should be written");
        std::fs::create_dir(root.join("chunks")).expect("chunk directory should be created");
        std::fs::write(root.join("chunks/0"), b"pixels").expect("chunk should be written");

        let file = InputReport::from_file("profile", &root.join("z.json"))
            .expect("regular file should hash");
        assert_eq!(file.bytes, 8);
        let directory =
            InputReport::from_path("raster", &root).expect("directory should hash recursively");
        assert_eq!(directory.bytes, 14);
        let repeated = InputReport::from_path("raster", &root)
            .expect("unchanged directory should hash identically");
        assert_eq!(directory.sha256, repeated.sha256);
        assert_eq!(sha256_bytes(b"metadata"), file.sha256);

        let missing = InputReport::from_path("raster", &root.join("missing"))
            .expect_err("missing input should fail");
        assert_eq!(missing.code, "INPUT_READ_FAILED");
    }

    #[cfg(unix)]
    #[test]
    fn directory_input_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory should be created");
        let root = temporary.path().join("array.zarr");
        std::fs::create_dir(&root).expect("input directory should be created");
        std::fs::write(temporary.path().join("outside"), b"pixels")
            .expect("target should be written");
        symlink(temporary.path().join("outside"), root.join("chunk"))
            .expect("test symlink should be created");

        let error = InputReport::from_path("raster", &root)
            .expect_err("symlinked input should fail closed");
        assert_eq!(error.code, "INPUT_PATH_INVALID");
        assert!(error.message.contains("symbolic link"));
    }

    #[test]
    fn report_writers_emit_one_json_object_and_surface_sink_failures() {
        let temporary = tempfile::tempdir().expect("temporary directory should be created");
        let object_path = temporary.path().join("ann.dcm");
        std::fs::write(&object_path, b"dicom bytes").expect("output fixture should be written");
        let output = OutputReport::for_file(
            "ann",
            &object_path,
            Path::new("ann.dcm"),
            "1.2.840.10008.5.1.4.1.1.91.1",
            "2.25.1",
            "2.25.2",
        )
        .expect("output report should hash the written object");
        assert_eq!(output.bytes, 11);

        let mut report = SuccessReport::new("convert-geojson");
        report
            .inputs
            .push(InputReport::from_bytes("mapping", Path::new("mapping.json"), b"{}").unwrap());
        report.outputs.push(output);
        report
            .target_coverage
            .push(CoverageReport::features("ann", 1));
        report.target_coverage.push(CoverageReport::raster(2, 1));
        report.semantic_digest = "abc".into();
        report.record_verification_completion(Instant::now(), 4096);

        let mut stdout = Vec::new();
        assert_eq!(emit_success(&report, &mut stdout), 0);
        assert_eq!(stdout.iter().filter(|byte| **byte == b'\n').count(), 1);
        let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["status"], "ok");
        assert_eq!(value["peak_tracked_heap_bytes"], 4096);

        let manifest = temporary.path().join("manifest.json");
        write_manifest(&manifest, &report).expect("manifest should serialize");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(manifest).unwrap()).unwrap()
                ["semantic_digest"],
            "abc"
        );

        let error = ConversionError::new("BAD_INPUT", "broken input");
        let mut error_stdout = Vec::new();
        let mut error_stderr = Vec::new();
        assert_eq!(
            emit_error(
                "convert-geojson",
                &error,
                &mut error_stdout,
                &mut error_stderr
            ),
            1
        );
        assert!(String::from_utf8(error_stderr)
            .unwrap()
            .contains("broken input"));

        let mut usage_stdout = Vec::new();
        let mut usage_stderr = Vec::new();
        assert_eq!(
            emit_usage_error(
                "convert-raster",
                "missing profile",
                "usage text",
                &mut usage_stdout,
                &mut usage_stderr,
            ),
            2
        );
        assert!(String::from_utf8(usage_stderr)
            .unwrap()
            .contains("usage text"));

        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("sink failed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(emit_success(&report, BrokenWriter), 1);
        assert_eq!(
            emit_error("convert-geojson", &error, BrokenWriter, io::sink()),
            1
        );
        assert_eq!(
            emit_usage_error(
                "convert-raster",
                "missing profile",
                "usage text",
                BrokenWriter,
                io::sink(),
            ),
            1
        );
    }
}
