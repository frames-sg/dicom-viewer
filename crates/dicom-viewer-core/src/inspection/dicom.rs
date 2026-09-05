use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use dicom_core::Tag;
use dicom_dictionary_std::{tags, uids};
use dicom_object::DefaultDicomObject;

use crate::{DicomInstanceSummary, LevelInfo, LevelTileLayout, Result, SourceKind, ViewerError};

const MAX_FOLDER_INSPECTION_FILES: usize = 100_000;
#[cfg(test)]
pub(crate) use wsi_dicom_annotations::metadata::{
    MAX_METADATA_ELEMENT_BYTES, MAX_METADATA_SEQUENCE_DEPTH, MAX_METADATA_VALUE_BYTES,
};

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
    wsi_dicom_annotations::metadata::open_metadata_object(path).map_err(|error| match error {
        // Preserve the viewer's established error variants at this shared boundary.
        wsi_dicom_annotations::Error::Io { path, source } => ViewerError::Io { path, source },
        wsi_dicom_annotations::Error::DicomRead { path, source } => {
            ViewerError::DicomRead { path, source }
        }
        wsi_dicom_annotations::Error::InvalidInput(reason) => ViewerError::InvalidInput(reason),
        other => ViewerError::Annotation(other),
    })
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
