use std::fs;
use std::io::Read;
use std::path::Path;

pub(super) fn read_bounded(
    path: &Path,
    maximum: u64,
    description: &str,
) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "could not inspect {description} {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(format!(
            "{description} {} must be a regular file no larger than {maximum} bytes",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .map_err(|_| format!("{description} length does not fit this platform"))?,
    );
    fs::File::open(path)
        .map_err(|error| format!("could not open {description} {}: {error}", path.display()))?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read {description} {}: {error}", path.display()))?;
    if bytes.len() as u64 > maximum {
        return Err(format!(
            "{description} {} changed while being read or exceeds its limit",
            path.display()
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_reports_missing_oversized_and_non_file_inputs() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("profile.json");
        std::fs::write(&file, b"1234").unwrap();

        assert_eq!(read_bounded(&file, 4, "profile").unwrap(), b"1234");
        let oversized = read_bounded(&file, 3, "profile").unwrap_err();
        assert!(oversized.contains("no larger than 3 bytes"));

        let missing = read_bounded(&directory.path().join("missing"), 4, "profile").unwrap_err();
        assert!(missing.contains("could not inspect profile"));
        let non_file = read_bounded(directory.path(), u64::MAX, "profile").unwrap_err();
        assert!(non_file.contains("must be a regular file"));
    }
}
