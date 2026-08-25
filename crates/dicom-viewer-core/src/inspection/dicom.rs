use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use dicom_core::{Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_encoding::transfer_syntax::{TransferSyntax, TransferSyntaxIndex};
use dicom_object::{file::ReadPreamble, DefaultDicomObject, OpenFileOptions};
use dicom_parser::dataset::{lazy_read::LazyDataSetReader, LazyDataToken};
use dicom_transfer_syntax_registry::TransferSyntaxRegistry;

use crate::{DicomInstanceSummary, LevelInfo, LevelTileLayout, Result, SourceKind, ViewerError};

const MAX_FOLDER_INSPECTION_FILES: usize = 100_000;
const MAX_FILE_META_BYTES: u32 = 1024 * 1024;
const MAX_FILE_META_ELEMENTS: usize = 128;
pub(crate) const MAX_METADATA_ELEMENT_BYTES: u32 = 16 * 1024 * 1024;
pub(crate) const MAX_METADATA_VALUE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_METADATA_TOKENS: usize = 2_000_000;
pub(crate) const MAX_METADATA_SEQUENCE_DEPTH: usize = 64;
const MAX_TRANSFER_SYNTAX_UID_BYTES: u32 = 128;

#[derive(Debug)]
pub(crate) struct InputInspection {
    pub(crate) source_kind: SourceKind,
    pub(crate) file_count: usize,
    pub(crate) instances: Vec<DicomInstanceSummary>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn inspect_input(path: &Path) -> Result<InputInspection> {
    let metadata = std::fs::metadata(path).map_err(|source| ViewerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let source_kind = if metadata.is_dir() {
        SourceKind::Folder
    } else if metadata.is_file() {
        SourceKind::File
    } else {
        return Err(ViewerError::InvalidInput(format!(
            "{} is neither a file nor a folder",
            path.display()
        )));
    };

    let candidates = candidate_paths(path, source_kind)?;
    let file_count = candidates.len();
    let mut instances = Vec::new();
    let mut warnings = Vec::new();
    let mut series_instance_uids = BTreeSet::new();
    for candidate in candidates {
        let likely_dicom = likely_dicom_path(&candidate);
        if source_kind == SourceKind::File && !likely_dicom && !has_dicom_preamble(&candidate)? {
            continue;
        }
        match inspect_dicom_instance(&candidate) {
            Ok(Some((instance, series_instance_uid))) => {
                if let Some(uid) = series_instance_uid {
                    series_instance_uids.insert(uid);
                }
                instances.push(instance);
            }
            Ok(None) => {
                if likely_dicom {
                    warnings.push(format!(
                        "ignored non-WSI DICOM file {}",
                        file_name_display(&candidate)
                    ));
                }
            }
            Err(err) => {
                if likely_dicom {
                    warnings.push(format!(
                        "ignored unreadable DICOM candidate {}: {err}",
                        file_name_display(&candidate)
                    ));
                }
            }
        }
    }

    instances.sort_by(|a, b| a.path.cmp(&b.path));
    if series_instance_uids.len() > 1 {
        return Err(ViewerError::InvalidInput(format!(
            "folder contains {} distinct DICOM series; open one series folder at a time",
            series_instance_uids.len()
        )));
    }
    Ok(InputInspection {
        source_kind,
        file_count,
        instances,
        warnings,
    })
}

fn likely_dicom_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "dcm" | "dicom"))
}

fn has_dicom_preamble(path: &Path) -> Result<bool> {
    let mut file = std::fs::File::open(path).map_err(|source| ViewerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut preamble = [0u8; 132];
    let read = file.read(&mut preamble).map_err(|source| ViewerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(read == preamble.len() && &preamble[128..] == b"DICM")
}

fn candidate_paths(path: &Path, source_kind: SourceKind) -> Result<Vec<PathBuf>> {
    candidate_paths_with_limit(path, source_kind, MAX_FOLDER_INSPECTION_FILES)
}

pub(crate) fn candidate_paths_with_limit(
    path: &Path,
    source_kind: SourceKind,
    max_folder_files: usize,
) -> Result<Vec<PathBuf>> {
    match source_kind {
        SourceKind::File => Ok(vec![path.to_path_buf()]),
        SourceKind::Folder => {
            let mut paths = Vec::new();
            for entry in std::fs::read_dir(path).map_err(|source| ViewerError::Io {
                path: path.to_path_buf(),
                source,
            })? {
                let entry = entry.map_err(|source| ViewerError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                let entry_path = entry.path();
                if entry_path.is_file() {
                    if paths.len() >= max_folder_files {
                        return Err(ViewerError::InvalidInput(format!(
                            "folder {} contains more than {max_folder_files} files; select a single WSI series folder",
                            path.display()
                        )));
                    }
                    paths.push(entry_path);
                }
            }
            paths.sort();
            Ok(paths)
        }
    }
}

pub(crate) fn open_metadata_object(path: &Path) -> Result<DefaultDicomObject> {
    let mut file = std::fs::File::open(path).map_err(|source| ViewerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let transfer_syntax_uid = preflight_file_meta(&mut file, path)?;
    let transfer_syntax = TransferSyntaxRegistry
        .get(&transfer_syntax_uid)
        .ok_or_else(|| {
            invalid_metadata(
                path,
                format!("unsupported transfer syntax {transfer_syntax_uid}"),
            )
        })?;
    preflight_data_set(&mut file, path, transfer_syntax)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| ViewerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    OpenFileOptions::new()
        .read_until(tags::FLOAT_PIXEL_DATA)
        .read_preamble(ReadPreamble::Always)
        .from_reader(file)
        .map_err(|source| ViewerError::DicomRead {
            path: path.to_path_buf(),
            source: Box::new(source),
        })
}

fn preflight_file_meta(file: &mut std::fs::File, path: &Path) -> Result<String> {
    file.seek(SeekFrom::Start(128))
        .map_err(|source| ViewerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut magic = [0_u8; 4];
    read_metadata_exact(file, path, &mut magic, "DICOM magic code")?;
    if &magic != b"DICM" {
        return Err(invalid_metadata(path, "missing DICOM file preamble"));
    }

    let group_length_header = read_explicit_header(file, path)?;
    if group_length_header.tag != tags::FILE_META_INFORMATION_GROUP_LENGTH
        || group_length_header.vr != VR::UL
        || group_length_header.value_len != 4
    {
        return Err(invalid_metadata(
            path,
            "invalid File Meta Information Group Length element",
        ));
    }
    let mut length_bytes = [0_u8; 4];
    read_metadata_exact(file, path, &mut length_bytes, "file meta group length")?;
    let group_length = u32::from_le_bytes(length_bytes);
    if group_length > MAX_FILE_META_BYTES {
        return Err(invalid_metadata(
            path,
            format!(
                "file meta group is {group_length} bytes, exceeding the {MAX_FILE_META_BYTES}-byte limit"
            ),
        ));
    }

    let mut remaining = u64::from(group_length);
    let mut element_count = 0_usize;
    let mut transfer_syntax_uid = None;
    while remaining > 0 {
        element_count = element_count.saturating_add(1);
        if element_count > MAX_FILE_META_ELEMENTS {
            return Err(invalid_metadata(
                path,
                format!("file meta group exceeds the {MAX_FILE_META_ELEMENTS}-element limit"),
            ));
        }
        let header = read_explicit_header(file, path)?;
        let encoded_len = header
            .header_len
            .checked_add(u64::from(header.value_len))
            .ok_or_else(|| invalid_metadata(path, "file meta element length overflows"))?;
        if header.tag.group() != 0x0002 || encoded_len > remaining {
            return Err(invalid_metadata(
                path,
                format!(
                    "file meta element {} exceeds the declared group boundary",
                    header.tag
                ),
            ));
        }
        if header.tag == tags::TRANSFER_SYNTAX_UID {
            if header.value_len == 0 || header.value_len > MAX_TRANSFER_SYNTAX_UID_BYTES {
                return Err(invalid_metadata(
                    path,
                    "transfer syntax UID has an invalid declared length",
                ));
            }
            let mut value = vec![0_u8; header.value_len as usize];
            read_metadata_exact(file, path, &mut value, "transfer syntax UID")?;
            let value = String::from_utf8(value).map_err(|_| {
                invalid_metadata(path, "transfer syntax UID is not valid ASCII/UTF-8")
            })?;
            transfer_syntax_uid = Some(
                value
                    .trim_end_matches(|character: char| {
                        character.is_whitespace() || character == '\0'
                    })
                    .to_string(),
            );
        } else {
            file.seek(SeekFrom::Current(i64::from(header.value_len)))
                .map_err(|source| ViewerError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        remaining -= encoded_len;
    }

    let position = file.stream_position().map_err(|source| ViewerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let file_len = file
        .metadata()
        .map_err(|source| ViewerError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    if position > file_len {
        return Err(invalid_metadata(
            path,
            "file meta group extends beyond the end of the file",
        ));
    }
    transfer_syntax_uid
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| invalid_metadata(path, "file meta group has no transfer syntax UID"))
}

struct ExplicitHeader {
    tag: Tag,
    vr: VR,
    value_len: u32,
    header_len: u64,
}

fn read_explicit_header(file: &mut std::fs::File, path: &Path) -> Result<ExplicitHeader> {
    let mut base = [0_u8; 8];
    read_metadata_exact(file, path, &mut base, "file meta element header")?;
    let tag = Tag(
        u16::from_le_bytes([base[0], base[1]]),
        u16::from_le_bytes([base[2], base[3]]),
    );
    let vr = VR::from_binary([base[4], base[5]])
        .ok_or_else(|| invalid_metadata(path, format!("invalid VR in file meta element {tag}")))?;
    let uses_u32_length = matches!(
        vr,
        VR::OB
            | VR::OD
            | VR::OF
            | VR::OL
            | VR::OV
            | VR::OW
            | VR::SQ
            | VR::UC
            | VR::UN
            | VR::UR
            | VR::UT
    );
    let (value_len, header_len) = if uses_u32_length {
        if base[6..8] != [0, 0] {
            return Err(invalid_metadata(
                path,
                format!("invalid reserved bytes in file meta element {tag}"),
            ));
        }
        let mut length = [0_u8; 4];
        read_metadata_exact(file, path, &mut length, "file meta element length")?;
        (u32::from_le_bytes(length), 12)
    } else {
        (u32::from(u16::from_le_bytes([base[6], base[7]])), 8)
    };
    Ok(ExplicitHeader {
        tag,
        vr,
        value_len,
        header_len,
    })
}

fn preflight_data_set(
    file: &mut std::fs::File,
    path: &Path,
    transfer_syntax: &TransferSyntax,
) -> Result<()> {
    let mut reader = LazyDataSetReader::new_with_ts(file, transfer_syntax).map_err(|error| {
        invalid_metadata(
            path,
            format!("could not initialize metadata parser: {error}"),
        )
    })?;
    let mut token_count = 0_usize;
    let mut sequence_depth = 0_usize;
    let mut declared_value_bytes = 0_u64;

    while let Some(token) = reader.advance() {
        token_count = token_count.saturating_add(1);
        if token_count > MAX_METADATA_TOKENS {
            return Err(invalid_metadata(
                path,
                format!("metadata exceeds the {MAX_METADATA_TOKENS}-token limit"),
            ));
        }
        let token = token.map_err(|error| {
            invalid_metadata(path, format!("could not preflight metadata: {error}"))
        })?;
        match token {
            LazyDataToken::ElementHeader(header) => {
                if is_pixel_value(header.tag) {
                    return Ok(());
                }
                if header.len.0 > MAX_METADATA_ELEMENT_BYTES {
                    return Err(invalid_metadata(
                        path,
                        format!(
                            "metadata element value limit is {MAX_METADATA_ELEMENT_BYTES} bytes, but {} declares {} bytes",
                            header.tag, header.len.0
                        ),
                    ));
                }
                declared_value_bytes = declared_value_bytes
                    .checked_add(u64::from(header.len.0))
                    .ok_or_else(|| invalid_metadata(path, "metadata byte count overflows"))?;
                if declared_value_bytes > MAX_METADATA_VALUE_BYTES {
                    return Err(invalid_metadata(
                        path,
                        format!(
                            "metadata declares more than the {MAX_METADATA_VALUE_BYTES}-byte cumulative value limit"
                        ),
                    ));
                }
            }
            LazyDataToken::SequenceStart { tag, .. } => {
                if is_pixel_value(tag) {
                    return Ok(());
                }
                sequence_depth = sequence_depth.saturating_add(1);
                if sequence_depth > MAX_METADATA_SEQUENCE_DEPTH {
                    return Err(invalid_metadata(
                        path,
                        format!(
                            "metadata sequence nesting exceeds the {MAX_METADATA_SEQUENCE_DEPTH}-level limit"
                        ),
                    ));
                }
            }
            LazyDataToken::PixelSequenceStart => return Ok(()),
            LazyDataToken::SequenceEnd => {
                sequence_depth = sequence_depth.saturating_sub(1);
            }
            lazy @ (LazyDataToken::LazyValue { .. } | LazyDataToken::LazyItemValue { .. }) => {
                lazy.skip().map_err(|error| {
                    invalid_metadata(path, format!("could not skip metadata value: {error}"))
                })?;
            }
            LazyDataToken::ItemStart { .. } | LazyDataToken::ItemEnd => {}
            _ => {
                return Err(invalid_metadata(
                    path,
                    "metadata parser returned an unsupported token",
                ));
            }
        }
    }
    if sequence_depth != 0 {
        return Err(invalid_metadata(
            path,
            "metadata ended inside an unterminated sequence",
        ));
    }
    Ok(())
}

fn is_pixel_value(tag: Tag) -> bool {
    matches!(
        tag,
        tags::FLOAT_PIXEL_DATA | tags::DOUBLE_FLOAT_PIXEL_DATA | tags::PIXEL_DATA
    )
}

fn read_metadata_exact(
    file: &mut std::fs::File,
    path: &Path,
    bytes: &mut [u8],
    context: &str,
) -> Result<()> {
    file.read_exact(bytes)
        .map_err(|error| invalid_metadata(path, format!("could not read {context}: {error}")))
}

fn invalid_metadata(path: &Path, reason: impl std::fmt::Display) -> ViewerError {
    ViewerError::InvalidInput(format!(
        "DICOM metadata preflight failed for {}: {reason}",
        path.display()
    ))
}

fn inspect_dicom_instance(path: &Path) -> Result<Option<(DicomInstanceSummary, Option<String>)>> {
    let obj = open_metadata_object(path)?;
    let sop_class_uid = obj.meta().media_storage_sop_class_uid().to_string();
    if sop_class_uid != uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE {
        return Ok(None);
    }

    let series_instance_uid = optional_string(&obj, tags::SERIES_INSTANCE_UID);
    let concatenation_instance_count = if optional_string(&obj, tags::CONCATENATION_UID).is_some() {
        optional_u32(&obj, tags::IN_CONCATENATION_TOTAL_NUMBER)
    } else {
        Some(1)
    };
    Ok(Some((
        DicomInstanceSummary {
            path: path.to_path_buf(),
            sop_class_uid,
            series_instance_uid_present: series_instance_uid.is_some(),
            transfer_syntax_uid: obj.meta().transfer_syntax().to_string(),
            image_type: optional_string(&obj, tags::IMAGE_TYPE)
                .map(|raw| {
                    raw.split('\\')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            rows: optional_u32(&obj, tags::ROWS),
            columns: optional_u32(&obj, tags::COLUMNS),
            total_pixel_matrix_rows: optional_u32(&obj, tags::TOTAL_PIXEL_MATRIX_ROWS),
            total_pixel_matrix_columns: optional_u32(&obj, tags::TOTAL_PIXEL_MATRIX_COLUMNS),
            number_of_frames: optional_u32(&obj, tags::NUMBER_OF_FRAMES),
            optical_path_count: optional_u32(&obj, tags::NUMBER_OF_OPTICAL_PATHS),
            focal_plane_count: optional_u32(&obj, tags::NUMBER_OF_FOCAL_PLANES),
            concatenation_instance_count,
            pixel_spacing: optional_spacing(&obj),
            dimension_organization_type: optional_string(&obj, tags::DIMENSION_ORGANIZATION_TYPE),
            samples_per_pixel: optional_u32(&obj, tags::SAMPLES_PER_PIXEL),
            photometric_interpretation: optional_string(&obj, tags::PHOTOMETRIC_INTERPRETATION),
            planar_configuration: optional_u32(&obj, tags::PLANAR_CONFIGURATION),
            bits_allocated: optional_u32(&obj, tags::BITS_ALLOCATED),
            bits_stored: optional_u32(&obj, tags::BITS_STORED),
            high_bit: optional_u32(&obj, tags::HIGH_BIT),
            pixel_representation: optional_u32(&obj, tags::PIXEL_REPRESENTATION),
        },
        series_instance_uid,
    )))
}

fn optional_spacing(obj: &DefaultDicomObject) -> Option<(f64, f64)> {
    let spacing = obj
        .get(tags::PIXEL_SPACING)
        .and_then(|element| element.to_multi_float64().ok())?;
    if spacing.len() >= 2 {
        Some((spacing[1], spacing[0]))
    } else {
        None
    }
}

fn optional_string(object: &DefaultDicomObject, tag: Tag) -> Option<String> {
    object
        .get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_end_matches('\0').trim().to_string())
        .filter(|value| !value.is_empty())
}

fn optional_u32(object: &DefaultDicomObject, tag: Tag) -> Option<u32> {
    object
        .get(tag)
        .and_then(|element| element.to_int::<u32>().ok())
}

pub(crate) fn build_fact_warnings(
    instances: &[DicomInstanceSummary],
    levels: &[LevelInfo],
) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut transfer_syntaxes = BTreeSet::new();
    let mut image_types = BTreeMap::<Vec<String>, usize>::new();
    let mut series_uid_presence = BTreeSet::new();

    for instance in instances {
        transfer_syntaxes.insert(instance.transfer_syntax_uid.clone());
        *image_types.entry(instance.image_type.clone()).or_insert(0) += 1;
        series_uid_presence.insert(instance.series_instance_uid_present);

        if let (Some(expected_frames), Some(frames)) = (
            expected_tiled_full_frame_count(instance),
            instance.number_of_frames,
        ) {
            if expected_frames != u64::from(frames) {
                warnings.push(format!(
                    "{} declares {frames} frame{} but a dense TILED_FULL grid expects {expected_frames}",
                    file_name_display(&instance.path),
                    if frames == 1 { "" } else { "s" }
                ));
            }
        }
    }

    if transfer_syntaxes.len() > 1 {
        warnings.push(format!(
            "series uses {} transfer syntaxes; verify this was intentional",
            transfer_syntaxes.len()
        ));
    }
    if image_types.len() > 3 {
        warnings.push(format!(
            "series contains {} distinct image type groups",
            image_types.len()
        ));
    }
    if series_uid_presence.contains(&false) {
        warnings.push("one or more DICOM instances are missing SeriesInstanceUID".into());
    }
    if levels.len() == 1 {
        warnings.push("only one pyramid level was detected".into());
    }
    for level in levels {
        if let LevelTileLayout::Regular {
            tile_width,
            tile_height,
            ..
        } = level.tile_layout
        {
            if tile_width == 0 || tile_height == 0 {
                warnings.push(format!("level {} has zero tile dimensions", level.index));
            }
        }
    }

    warnings
}

fn expected_tiled_full_frame_count(instance: &DicomInstanceSummary) -> Option<u64> {
    if instance.dimension_organization_type.as_deref() != Some("TILED_FULL") {
        return None;
    }
    let columns = instance.columns?;
    let rows = instance.rows?;
    if columns == 0 || rows == 0 {
        return None;
    }
    let matrix_columns = instance.total_pixel_matrix_columns?;
    let matrix_rows = instance.total_pixel_matrix_rows?;
    let optical_paths = instance.optical_path_count?;
    let focal_planes = instance.focal_plane_count?;
    if optical_paths == 0 || focal_planes == 0 || instance.concatenation_instance_count? != 1 {
        return None;
    }
    u64::from(matrix_columns.div_ceil(columns))
        .checked_mul(u64::from(matrix_rows.div_ceil(rows)))?
        .checked_mul(u64::from(optical_paths))?
        .checked_mul(u64::from(focal_planes))
}

fn file_name_display(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), str::to_string)
}

#[cfg(test)]
#[path = "dicom_tests.rs"]
mod tests;
