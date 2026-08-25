use std::fs;
use std::io::Read;
use std::path::Path;

use wsi_dicom_annotations::DicomPublicationError;

use crate::conversion_report::ConversionError;

pub(crate) fn publication_error(error: DicomPublicationError) -> ConversionError {
    ConversionError::new(error.code(), error.to_string())
}

pub(crate) fn read_bounded_file(
    path: &Path,
    maximum_bytes: u64,
    description: &str,
) -> Result<Vec<u8>, ConversionError> {
    let metadata = fs::metadata(path)
        .map_err(|error| ConversionError::io("INPUT_READ_FAILED", path, error))?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return Err(ConversionError::new(
            "INPUT_SIZE_INVALID",
            format!(
                "{description} {} must be a file no larger than {maximum_bytes} bytes",
                path.display()
            ),
        ));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        ConversionError::new(
            "INPUT_SIZE_INVALID",
            format!("{description} length does not fit this platform"),
        )
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    fs::File::open(path)
        .map_err(|error| ConversionError::io("INPUT_READ_FAILED", path, error))?
        .take(maximum_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ConversionError::io("INPUT_READ_FAILED", path, error))?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(ConversionError::new(
            "INPUT_SIZE_INVALID",
            format!("{description} changed while being read or exceeds its size limit"),
        ));
    }
    Ok(bytes)
}
