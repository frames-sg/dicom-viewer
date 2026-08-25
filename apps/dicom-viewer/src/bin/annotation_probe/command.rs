use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use crate::{convert_geojson, convert_raster, legacy};

pub(crate) const USAGE: &str = "usage:\n  annotation_probe inspect --source <referenced-wsi> [--canonical-source <level0-wsi>] [--payload full|digest] <ann-or-seg>\n  annotation_probe roundtrip --source <referenced-wsi> [--canonical-source <level0-wsi>] --output <new.dcm> [--allow-lossy] [--payload full|digest] <ann-or-seg>\n  annotation_probe convert-geojson --source <wsi.dcm> [--canonical-source <level0-wsi.dcm>] --mapping <label-mapping-v1.json> --coordinate-space level0-pixels|source-pixels|slide-mm --target ann [--target seg] [--target sr] (--output <object.dcm> | --output-dir <bundle-dir>) [--allow-lossy] <annotations.geojson>\n  annotation_probe convert-raster --source <wsi.dcm> [--canonical-source <level0-wsi.dcm>] --profile <raster-profile-v1.json> [--channel <index-or-name> | --all-channels] (--output <map.dcm> | --output-dir <map-bundle-dir>) [--max-instance-bytes <bytes>] <raster-input>";

pub(crate) fn next_path(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{option} requires a path"))
}

pub(crate) fn next_utf8(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<String, String> {
    let value = arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))?;
    value
        .into_string()
        .map_err(|_| format!("{option} requires UTF-8 text"))
}

pub(crate) fn set_once<T>(slot: &mut Option<T>, value: T, option: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{option} may be supplied only once"));
    }
    *slot = Some(value);
    Ok(())
}

pub(crate) fn execute(
    arguments: impl IntoIterator<Item = OsString>,
    stdout: impl Write,
    mut stderr: impl Write,
) -> i32 {
    let arguments: Vec<_> = arguments.into_iter().collect();
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("inspect" | "roundtrip") => legacy::execute(arguments, stdout, stderr),
        Some("convert-geojson") => {
            convert_geojson::execute(arguments.into_iter().skip(1), stdout, stderr)
        }
        Some("convert-raster") => {
            convert_raster::execute(arguments.into_iter().skip(1), stdout, stderr)
        }
        _ => {
            let _ = writeln!(
                stderr,
                "the first argument must name a supported command\n{USAGE}"
            );
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_commands_have_owned_grammars() {
        for (command, expected_error) in [
            ("convert-geojson", "--source is required"),
            ("convert-raster", "--source is required"),
        ] {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();

            let exit = execute([OsString::from(command)], &mut stdout, &mut stderr);

            assert_eq!(exit, 2);
            let report: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
            assert_eq!(report["schema"], "conversion-report-v1");
            assert_eq!(report["status"], "error");
            assert_eq!(report["operation"], command);
            assert_eq!(report["error"]["code"], "USAGE_ERROR");
            let stderr = String::from_utf8(stderr).unwrap();
            assert!(stderr.contains(expected_error), "{stderr}");
            assert!(stderr.contains("annotation_probe convert-geojson"));
            assert!(stderr.contains("annotation_probe convert-raster"));
        }
    }

    #[test]
    fn conversion_failures_emit_conversion_report_v1() {
        for (command, profile_option, profile_path, input) in [
            (
                "convert-geojson",
                "--mapping",
                "missing-mapping.json",
                "missing.geojson",
            ),
            (
                "convert-raster",
                "--profile",
                "missing-profile.json",
                "missing.npy",
            ),
        ] {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut arguments = vec![
                OsString::from(command),
                OsString::from("--source"),
                OsString::from("missing-source.dcm"),
                OsString::from(profile_option),
                OsString::from(profile_path),
            ];
            if command == "convert-geojson" {
                arguments.extend([
                    OsString::from("--coordinate-space"),
                    OsString::from("level0-pixels"),
                    OsString::from("--target"),
                    OsString::from("ann"),
                ]);
            }
            arguments.extend([
                OsString::from("--output"),
                OsString::from("missing-output.dcm"),
                OsString::from(input),
            ]);

            let exit = execute(arguments, &mut stdout, &mut stderr);

            assert_eq!(exit, 1, "{}", String::from_utf8_lossy(&stderr));
            let report: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
            assert_eq!(report["schema"], "conversion-report-v1");
            assert_eq!(report["schema_version"], 1);
            assert_eq!(report["status"], "error");
            assert_eq!(report["operation"], command);
            assert_eq!(report["error"]["code"], "SOURCE_READ_FAILED");
            assert!(!stderr.is_empty());
        }
    }
}
