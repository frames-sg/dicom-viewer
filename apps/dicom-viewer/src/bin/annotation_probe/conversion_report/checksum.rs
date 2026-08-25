use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::ConversionError;

pub(super) fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn sha256_file(path: &Path) -> Result<(u64, String), ConversionError> {
    let mut file =
        File::open(path).map_err(|error| ConversionError::io("INPUT_READ_FAILED", path, error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    let mut bytes = 0_u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| ConversionError::io("INPUT_READ_FAILED", path, error))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        bytes = bytes
            .checked_add(u64::try_from(count).map_err(|_| {
                ConversionError::new("INPUT_TOO_LARGE", "input read length does not fit u64")
            })?)
            .ok_or_else(|| ConversionError::new("INPUT_TOO_LARGE", "input size overflow"))?;
    }
    Ok((bytes, format!("{:x}", digest.finalize())))
}

pub(super) fn sha256_directory(path: &Path) -> Result<(u64, String), ConversionError> {
    let mut digest = Sha256::new();
    digest.update(b"conversion-input-directory-v1\0");
    let mut bytes = 0_u64;
    let mut entry_count = 0_u64;
    hash_directory(path, path, 0, &mut entry_count, &mut bytes, &mut digest)?;
    Ok((bytes, format!("{:x}", digest.finalize())))
}

fn hash_directory(
    root: &Path,
    directory: &Path,
    depth: u16,
    entry_count: &mut u64,
    bytes: &mut u64,
    digest: &mut Sha256,
) -> Result<(), ConversionError> {
    if depth > 128 {
        return Err(ConversionError::new(
            "INPUT_PATH_INVALID",
            format!(
                "input directory nesting exceeds 128 levels: {}",
                root.display()
            ),
        ));
    }
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| ConversionError::io("INPUT_READ_FAILED", directory, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| ConversionError::io("INPUT_READ_FAILED", directory, error))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        *entry_count = entry_count.checked_add(1).ok_or_else(|| {
            ConversionError::new("INPUT_TOO_LARGE", "input directory entry count overflow")
        })?;
        if *entry_count > 1_000_000 {
            return Err(ConversionError::new(
                "INPUT_TOO_LARGE",
                "input directory contains more than 1,000,000 entries",
            ));
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| ConversionError::io("INPUT_READ_FAILED", &path, error))?;
        let relative = path.strip_prefix(root).map_err(|_| {
            ConversionError::new(
                "INPUT_PATH_INVALID",
                format!("input entry escaped its root: {}", path.display()),
            )
        })?;
        let relative = relative.to_str().ok_or_else(|| {
            ConversionError::new(
                "INPUT_PATH_INVALID",
                format!(
                    "input directory contains a non-UTF-8 path: {}",
                    path.display()
                ),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ConversionError::new(
                "INPUT_PATH_INVALID",
                format!(
                    "input directory contains a symbolic link: {}",
                    path.display()
                ),
            ));
        }
        hash_component(
            digest,
            if metadata.is_dir() { b'D' } else { b'F' },
            relative,
        )?;
        if metadata.is_dir() {
            hash_directory(root, &path, depth + 1, entry_count, bytes, digest)?;
        } else if metadata.is_file() {
            let (length, file_digest) = sha256_file(&path)?;
            *bytes = bytes.checked_add(length).ok_or_else(|| {
                ConversionError::new("INPUT_TOO_LARGE", "input directory byte count overflow")
            })?;
            digest.update(length.to_le_bytes());
            digest.update(file_digest.as_bytes());
        } else {
            return Err(ConversionError::new(
                "INPUT_PATH_INVALID",
                format!(
                    "input directory contains a special file: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

fn hash_component(digest: &mut Sha256, kind: u8, relative: &str) -> Result<(), ConversionError> {
    let length = u64::try_from(relative.len()).map_err(|_| {
        ConversionError::new("INPUT_TOO_LARGE", "input path length does not fit u64")
    })?;
    digest.update([kind]);
    digest.update(length.to_le_bytes());
    digest.update(relative.as_bytes());
    Ok(())
}
