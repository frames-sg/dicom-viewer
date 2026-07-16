use std::path::Path;

use wsi_rs::{Dataset, PlaneIdx, SampleType, SceneId, SeriesId, Slide, TileLayout};

use crate::model::SelectedView;
use crate::{
    LevelIndex, LevelInfo, LevelTileLayout, Result, StudySummary, TileDecodeBackend, ViewerError,
};

use super::dicom::{build_fact_warnings, InputInspection};

pub(crate) fn summarize_slide(
    path: &Path,
    slide: &Slide,
    input: InputInspection,
    tile_decode_backend: TileDecodeBackend,
) -> Result<(StudySummary, SelectedView)> {
    let dataset = slide.dataset();
    let (selected_view, mut warnings) = select_primary_view(dataset)?;
    let series = &dataset.scenes[selected_view.scene.get()].series[selected_view.series.get()];
    let (levels, level_warnings) = summarize_renderable_levels(series)?;
    warnings.extend(level_warnings);

    let format_label = format_label(
        path,
        dataset.properties.vendor(),
        !input.instances.is_empty(),
    );
    warnings.extend(input.warnings);
    warnings.extend(build_fact_warnings(&input.instances, &levels));

    Ok((
        StudySummary {
            source_path: path.to_path_buf(),
            source_kind: input.source_kind,
            format_label,
            tile_decode_backend,
            file_count: input.file_count,
            dicom_instance_count: input.instances.len(),
            levels,
            instances: input.instances,
            warnings,
            mpp: dataset.properties.mpp(),
            objective_power: dataset.properties.objective_power(),
        },
        selected_view,
    ))
}

pub(crate) fn summarize_renderable_levels(
    series: &wsi_rs::Series,
) -> Result<(Vec<LevelInfo>, Vec<String>)> {
    let mut levels = Vec::new();
    let mut irregular_levels = 0usize;
    let mut invalid_levels = 0usize;
    for (index, level) in series.levels.iter().enumerate() {
        let index = LevelIndex::from_usize(index)?;
        let tile_layout = match &level.tile_layout {
            TileLayout::Regular {
                tile_width,
                tile_height,
                tiles_across,
                tiles_down,
            } => {
                if level.dimensions.0 == 0
                    || level.dimensions.1 == 0
                    || *tile_width == 0
                    || *tile_height == 0
                    || *tiles_across == 0
                    || *tiles_down == 0
                {
                    invalid_levels += 1;
                    continue;
                }
                LevelTileLayout::Regular {
                    tile_width: *tile_width,
                    tile_height: *tile_height,
                    tiles_across: *tiles_across,
                    tiles_down: *tiles_down,
                }
            }
            TileLayout::WholeLevel {
                width,
                height,
                virtual_tile_width,
                virtual_tile_height,
            } => {
                if *width == 0 || *height == 0 {
                    invalid_levels += 1;
                    continue;
                }
                LevelTileLayout::WholeLevel {
                    width: *width,
                    height: *height,
                    virtual_tile_width: *virtual_tile_width,
                    virtual_tile_height: *virtual_tile_height,
                }
            }
            TileLayout::Irregular { .. } => {
                irregular_levels += 1;
                continue;
            }
            #[allow(unreachable_patterns)]
            _ => {
                return Err(ViewerError::Unsupported(format!(
                    "level {index} uses a tile layout this viewer does not recognize"
                )));
            }
        };
        levels.push(LevelInfo {
            index,
            width: level.dimensions.0,
            height: level.dimensions.1,
            downsample: level.downsample,
            tile_layout,
        });
    }

    if levels.is_empty() {
        return Err(ViewerError::Unsupported(
            "WSI dataset has no regular or whole-level image levels the viewer can render".into(),
        ));
    }

    let mut warnings = Vec::new();
    if irregular_levels > 0 {
        warnings.push(format!(
            "skipped {irregular_levels} irregular-layout level{}; this viewer currently renders regular and whole-level layouts",
            if irregular_levels == 1 { "" } else { "s" }
        ));
    }
    if invalid_levels > 0 {
        warnings.push(format!(
            "skipped {invalid_levels} level{} with empty dimensions or tile grids",
            if invalid_levels == 1 { "" } else { "s" }
        ));
    }
    Ok((levels, warnings))
}

pub(crate) fn select_primary_view(dataset: &Dataset) -> Result<(SelectedView, Vec<String>)> {
    let scene = dataset
        .scenes
        .first()
        .ok_or_else(|| ViewerError::Unsupported("WSI dataset has no image scenes".into()))?;
    let series = scene
        .series
        .first()
        .ok_or_else(|| ViewerError::Unsupported("WSI dataset has no image series".into()))?;
    if !matches!(series.sample_type, SampleType::Uint8) {
        return Err(ViewerError::Unsupported(format!(
            "viewer supports Uint8 samples, but the selected series uses {:?}",
            series.sample_type
        )));
    }

    let mut warnings = Vec::new();
    if dataset.scenes.len() > 1 {
        warnings.push(format!(
            "dataset contains {} scenes; displaying scene 0",
            dataset.scenes.len()
        ));
    }
    if scene.series.len() > 1 {
        warnings.push(format!(
            "scene 0 contains {} series; displaying series 0",
            scene.series.len()
        ));
    }
    if series.axes.z > 1 || series.axes.c > 1 || series.axes.t > 1 {
        warnings.push(format!(
            "selected series has z/c/t dimensions {}/{}/{}; displaying the default 0/0/0 plane",
            series.axes.z, series.axes.c, series.axes.t
        ));
    }

    Ok((
        SelectedView {
            scene: SceneId::new(0),
            series: SeriesId::new(0),
            plane: PlaneIdx::default(),
        },
        warnings,
    ))
}

fn format_label(path: &Path, vendor: Option<&str>, has_dicom_instances: bool) -> String {
    if has_dicom_instances || vendor == Some("dicom") {
        return "DICOM VL WSI".into();
    }

    match vendor {
        Some("aperio") => "Aperio WSI".into(),
        Some("generic-tiff") => "Generic TIFF WSI".into(),
        Some("hamamatsu" | "hamamatsu-ndpi") => "Hamamatsu WSI".into(),
        Some("leica") => "Leica WSI".into(),
        Some("mirax") => "MIRAX WSI".into(),
        Some("olympus") => "Olympus WSI".into(),
        Some("philips") => "Philips TIFF WSI".into(),
        Some("raw-jp2k") => "Raw JPEG 2000 WSI".into(),
        Some("svcache") => "wsi-rs cache WSI".into(),
        Some("zeiss") => "Zeiss WSI".into(),
        Some(other) if !other.is_empty() => format!("{other} WSI"),
        _ => format_label_from_extension(path).unwrap_or_else(|| "wsi-rs WSI".into()),
    }
}

fn format_label_from_extension(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let label = match extension.as_str() {
        "bif" => "Ventana/Roche TIFF WSI",
        "czi" | "zvi" => "Zeiss WSI",
        "dcm" | "dicom" => "DICOM VL WSI",
        "j2c" | "j2k" => "Raw JPEG 2000 WSI",
        "mrxs" => "MIRAX WSI",
        "ndpi" | "vms" | "vmu" => "Hamamatsu WSI",
        "scn" => "Leica WSI",
        "svcache" => "wsi-rs cache WSI",
        "svs" => "Aperio WSI",
        "tif" | "tiff" => "TIFF WSI",
        "vsi" => "Olympus WSI",
        _ => return None,
    };
    Some(label.into())
}
