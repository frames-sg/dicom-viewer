use std::path::PathBuf;

use super::*;
use dicom_core::value::PrimitiveValue;
#[cfg(target_os = "macos")]
use dicom_core::value::{fragments::Fragments, PixelFragmentSequence, Value};
use dicom_core::{DataElement, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use crate::inspection::{
    build_fact_warnings, candidate_paths_with_limit, inspect_input, open_metadata_object,
    select_primary_view, summarize_renderable_levels,
};

#[test]
fn jp2k_cpu_decode_budget_leaves_one_available_processor_for_the_viewer() {
    assert_eq!(jp2k_cpu_decode_thread_budget(1).get(), 1);
    assert_eq!(jp2k_cpu_decode_thread_budget(2).get(), 1);
    assert_eq!(jp2k_cpu_decode_thread_budget(12).get(), 11);
}

#[test]
fn opened_study_applies_the_jp2k_cpu_decode_budget_to_wsi_rs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, j2k_test_support::htj2k_rgb8_fixture(16, 16)).unwrap();

    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();

    let expected = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .saturating_sub(1)
        .max(1);
    assert_eq!(
        study
            .slide
            .decode_execution_options()
            .jp2k_cpu_threads()
            .map(std::num::NonZeroUsize::get),
        Some(expected)
    );
}

#[test]
fn extracts_synthetic_wsi_facts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.dcm");
    write_test_dicom(
        &path,
        "1.2.826.0.1.3680043.10.777.1",
        "1.2.826.0.1.3680043.10.777",
    );

    let inspection = inspect_input(&path).unwrap();
    assert_eq!(inspection.instances.len(), 1);
    let instance = &inspection.instances[0];
    assert_eq!(instance.rows, Some(2));
    assert_eq!(instance.columns, Some(2));
    assert_eq!(instance.number_of_frames, Some(1));
    assert_eq!(
        instance.dimension_organization_type.as_deref(),
        Some("TILED_FULL")
    );
    assert_eq!(instance.samples_per_pixel, Some(3));
    assert_eq!(instance.photometric_interpretation.as_deref(), Some("RGB"));
    assert_eq!(instance.bits_stored, Some(8));
    assert_eq!(
        instance.transfer_syntax_uid,
        uids::EXPLICIT_VR_LITTLE_ENDIAN
    );
}

#[test]
fn metadata_preflight_stops_before_pixel_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.dcm");
    write_test_dicom(
        &path,
        "1.2.826.0.1.3680043.10.777.1",
        "1.2.826.0.1.3680043.10.777",
    );

    let obj = open_metadata_object(&path).unwrap();
    assert!(obj.get(tags::PIXEL_DATA).is_none());
}

#[test]
fn rejects_non_dicom_input_without_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-dicom.dcm");
    std::fs::write(&path, b"not dicom").unwrap();

    let err = ViewerStudy::open_path(&path).unwrap_err();
    assert!(
        matches!(err, ViewerError::Wsi(_)),
        "unexpected error: {err:?}"
    );
}

#[test]
fn opens_wsi_rs_raw_jp2k_without_dicom_instances() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, j2k_test_support::htj2k_rgb8_fixture(32, 24)).unwrap();

    let study = ViewerStudy::open_path(&path).unwrap();
    let summary = study.summary();
    assert_eq!(summary.format_label, "Raw JPEG 2000 WSI");
    assert_eq!(summary.file_count, 1);
    assert_eq!(summary.dicom_instance_count, 0);
    assert_eq!(summary.levels.len(), 1);
    assert_eq!(study.selected_view.scene.get(), 0);
    assert_eq!(study.selected_view.series.get(), 0);
    assert_eq!(study.selected_view.plane, wsi_rs::PlaneIdx::default());

    let tile = study
        .read_tile_rgba(LevelIndex::from_u32(0), TileCoord::new(0, 0))
        .unwrap();
    assert_eq!((tile.width, tile.height), (32, 24));
    assert_eq!(tile.rgba.len(), 32 * 24 * 4);
}

#[test]
fn render_tile_api_preserves_cpu_output_and_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, j2k_test_support::htj2k_rgb8_fixture(32, 24)).unwrap();
    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    let requests = [
        (LevelIndex::from_u32(0), TileCoord::new(0, 0)),
        (LevelIndex::from_u32(0), TileCoord::new(0, 0)),
    ];

    let tiles = study.read_tiles_for_render(&requests).unwrap();

    assert_eq!(tiles.len(), requests.len());
    for tile in tiles {
        let RenderTile::Cpu(tile) = tile else {
            panic!("CPU-only viewer options returned a device tile");
        };
        assert_eq!((tile.width, tile.height), (32, 24));
        assert_eq!(tile.rgba.len(), 32 * 24 * 4);
    }
    assert_eq!(study.summary().tile_decode_backend, TileDecodeBackend::Cpu);
}

#[test]
fn existing_rgba_api_remains_cpu_resident_with_render_options() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, j2k_test_support::htj2k_rgb8_fixture(16, 12)).unwrap();
    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::auto()).unwrap();

    let tile = study
        .read_tile_rgba(LevelIndex::from_u32(0), TileCoord::new(0, 0))
        .unwrap();

    assert_eq!((tile.width, tile.height), (16, 12));
    assert_eq!(tile.rgba.len(), 16 * 12 * 4);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_metal_options_return_resident_tiles_for_synthetic_dicom_htj2k() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metal.dcm");
    write_test_htj2k_dicom(&path, j2k_test_support::htj2k_rgb8_fixture(2, 2));
    let study = ViewerStudy::open_path_with_options(
        &path,
        ViewerOpenOptions::auto().with_metal_device(device.clone()),
    )
    .unwrap();
    let requests = (0..8)
        .map(|col| (LevelIndex::from_u32(0), TileCoord::new(col, 0)))
        .collect::<Vec<_>>();
    let raw_requests = requests
        .iter()
        .map(|&(level, coord)| build_tile_request(study.selected_view, level, coord).unwrap())
        .collect::<Vec<_>>();
    let required = study
        .slide
        .read_tiles(
            &raw_requests,
            wsi_rs::TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(
                wsi_rs::output::metal::MetalBackendSessions::new(device),
            ),
        )
        .unwrap();
    assert!(required
        .iter()
        .all(|tile| matches!(tile, wsi_rs::TilePixels::Device(_))));

    let tiles = study.read_tiles_for_render(&requests).unwrap();

    assert_eq!(tiles.len(), 8);
    for tile in tiles {
        let RenderTile::Metal(tile) = tile else {
            panic!("Metal-preferred synthetic DICOM HTJ2K batch returned host pixels");
        };
        assert_eq!((tile.width(), tile.height()), (2, 2));
        assert!(tile.byte_len() >= 12);
    }
}

#[test]
fn primary_view_selection_warns_about_additional_views_and_planes() {
    use wsi_rs::{
        AxesShape, ChannelInfo, Dataset, DatasetId, Level, SampleType, Scene, Series, TileLayout,
    };

    let level = || {
        Level::new(
            (16, 16),
            1.0,
            TileLayout::Regular {
                tile_width: 16,
                tile_height: 16,
                tiles_across: 1,
                tiles_down: 1,
            },
        )
    };
    let series = |id: &str, axes| {
        Series::new(
            id,
            axes,
            vec![level()],
            SampleType::Uint8,
            vec![ChannelInfo::new()],
        )
    };
    let dataset = Dataset::new(
        DatasetId::new(7),
        vec![
            Scene::new(
                "first",
                vec![
                    series("primary", AxesShape::new(2, 3, 4)),
                    series("secondary", AxesShape::default()),
                ],
            ),
            Scene::new("second", vec![series("other", AxesShape::default())]),
        ],
    );

    let (selected, warnings) = select_primary_view(&dataset).unwrap();

    assert_eq!(selected.scene.get(), 0);
    assert_eq!(selected.series.get(), 0);
    assert_eq!(selected.plane, wsi_rs::PlaneIdx::default());
    assert!(warnings.iter().any(|warning| warning.contains("2 scenes")));
    assert!(warnings.iter().any(|warning| warning.contains("2 series")));
    assert!(warnings.iter().any(|warning| warning.contains("2/3/4")));
}

#[test]
fn primary_view_selection_rejects_unsupported_sample_type() {
    use wsi_rs::{
        AxesShape, ChannelInfo, Dataset, DatasetId, Level, SampleType, Scene, Series, TileLayout,
    };

    let dataset = Dataset::new(
        DatasetId::new(8),
        vec![Scene::new(
            "scene",
            vec![Series::new(
                "series",
                AxesShape::default(),
                vec![Level::new(
                    (1, 1),
                    1.0,
                    TileLayout::Regular {
                        tile_width: 1,
                        tile_height: 1,
                        tiles_across: 1,
                        tiles_down: 1,
                    },
                )],
                SampleType::Uint16,
                vec![ChannelInfo::new()],
            )],
        )],
    );

    let err = select_primary_view(&dataset).unwrap_err();

    assert!(
        matches!(&err, ViewerError::Unsupported(message) if message.contains("Uint8") && message.contains("Uint16")),
        "unexpected error: {err:?}"
    );
}

#[test]
fn tile_requests_use_the_selected_scene_series_and_plane() {
    let plane = wsi_rs::PlaneIdx::new(wsi_rs::PlaneSelection::new(2, 3, 4));
    let selected = crate::model::SelectedView {
        scene: wsi_rs::SceneId::new(5),
        series: wsi_rs::SeriesId::new(6),
        plane,
    };
    let level = LevelIndex::from_u32(7);
    let coord = TileCoord::new(8, 9);

    let source = build_tile_request(selected, level, coord).unwrap();
    let display = build_tile_view_request(selected, level, coord, 512, 256).unwrap();

    assert_eq!(source.scene.get(), 5);
    assert_eq!(source.series.get(), 6);
    assert_eq!(source.level.get(), 7);
    assert_eq!(source.plane, plane);
    assert_eq!((source.col, source.row), (8, 9));
    assert_eq!(display.scene.get(), 5);
    assert_eq!(display.series.get(), 6);
    assert_eq!(display.level.get(), 7);
    assert_eq!(display.plane, plane);
    assert_eq!((display.col, display.row), (8, 9));
    assert_eq!((display.tile_width, display.tile_height), (512, 256));
}

#[test]
fn renderable_levels_skip_known_irregular_layouts_without_renumbering() {
    use std::collections::HashMap;
    use wsi_rs::{AxesShape, ChannelInfo, Level, SampleType, Series, TileLayout};

    let series = Series::new(
        "series",
        AxesShape::default(),
        vec![
            Level::new(
                (512, 512),
                1.0,
                TileLayout::Irregular {
                    tile_advance: (256.0, 256.0),
                    extra_tiles: (0, 0, 0, 0),
                    tiles: HashMap::new(),
                },
            ),
            Level::new(
                (128, 128),
                4.0,
                TileLayout::Regular {
                    tile_width: 128,
                    tile_height: 128,
                    tiles_across: 1,
                    tiles_down: 1,
                },
            ),
        ],
        SampleType::Uint8,
        vec![ChannelInfo::new()],
    );

    let (levels, warnings) = summarize_renderable_levels(&series).unwrap();

    assert_eq!(levels.len(), 1);
    assert_eq!(levels[0].index, LevelIndex::from_u32(1));
    assert!(warnings
        .iter()
        .any(|warning| warning.contains("1 irregular-layout level")));
}

#[test]
fn irregular_only_series_is_rejected_during_open_summary() {
    use std::collections::HashMap;
    use wsi_rs::{AxesShape, ChannelInfo, Level, SampleType, Series, TileLayout};

    let series = Series::new(
        "series",
        AxesShape::default(),
        vec![Level::new(
            (512, 512),
            1.0,
            TileLayout::Irregular {
                tile_advance: (256.0, 256.0),
                extra_tiles: (0, 0, 0, 0),
                tiles: HashMap::new(),
            },
        )],
        SampleType::Uint8,
        vec![ChannelInfo::new()],
    );

    let err = summarize_renderable_levels(&series).unwrap_err();

    assert!(
        matches!(&err, ViewerError::Unsupported(message) if message.contains("no regular or whole-level")),
        "unexpected error: {err:?}"
    );
}

#[test]
fn folder_scan_rejects_mixed_series_candidates() {
    let dir = tempfile::tempdir().unwrap();
    write_test_dicom(
        &dir.path().join("a.dcm"),
        "1.2.826.0.1.3680043.10.777.1",
        "1.2.826.0.1.3680043.10.777",
    );
    write_test_dicom(
        &dir.path().join("b.dcm"),
        "1.2.826.0.1.3680043.10.778.1",
        "1.2.826.0.1.3680043.10.778",
    );

    let err = inspect_input(dir.path()).unwrap_err();
    assert!(
        matches!(&err, ViewerError::InvalidInput(message) if message.contains("2 distinct DICOM series")),
        "mixed folder scan should fail before opening pixels: {err:?}"
    );
}

#[test]
fn non_dicom_single_file_skips_dicom_metadata_inspection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, b"not a DICOM file").unwrap();

    let inspection = inspect_input(&path).unwrap();

    assert!(inspection.instances.is_empty());
    assert!(inspection.warnings.is_empty());
}

#[test]
fn folder_scan_rejects_work_above_the_file_limit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.dcm"), b"first").unwrap();
    std::fs::write(dir.path().join("b.dcm"), b"second").unwrap();

    let err = candidate_paths_with_limit(dir.path(), SourceKind::Folder, 1).unwrap_err();
    assert!(
        matches!(&err, ViewerError::InvalidInput(message) if message.contains("more than 1 files")),
        "folder limit should return a clear invalid-input error: {err:?}"
    );
}

#[test]
fn warns_when_tiled_full_frame_count_mismatches_dense_grid() {
    let instance = DicomInstanceSummary {
        path: PathBuf::from("bad.dcm"),
        sop_class_uid: uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.into(),
        series_instance_uid_present: true,
        transfer_syntax_uid: uids::EXPLICIT_VR_LITTLE_ENDIAN.into(),
        image_type: vec![
            "ORIGINAL".into(),
            "PRIMARY".into(),
            "VOLUME".into(),
            "NONE".into(),
        ],
        rows: Some(2),
        columns: Some(2),
        total_pixel_matrix_rows: Some(5),
        total_pixel_matrix_columns: Some(5),
        number_of_frames: Some(8),
        pixel_spacing: None,
        dimension_organization_type: Some("TILED_FULL".into()),
        samples_per_pixel: Some(3),
        photometric_interpretation: Some("RGB".into()),
        planar_configuration: Some(0),
        bits_allocated: Some(8),
        bits_stored: Some(8),
        high_bit: Some(7),
        pixel_representation: Some(0),
    };
    let warnings = build_fact_warnings(&[instance], &[]);

    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("dense TILED_FULL grid expects 9")),
        "expected dense-grid frame warning, got {warnings:?}"
    );
}

#[test]
fn opens_and_reads_first_tile_through_wsi_rs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.dcm");
    write_test_dicom(
        &path,
        "1.2.826.0.1.3680043.10.777.1",
        "1.2.826.0.1.3680043.10.777",
    );

    let study = ViewerStudy::open_path(&path).unwrap();
    assert_eq!(study.summary().format_label, "DICOM VL WSI");
    assert_eq!(study.summary().file_count, 1);
    assert_eq!(study.summary().dicom_instance_count, 1);
    assert_eq!(study.summary().levels.len(), 1);
    let tile = study
        .read_tile_rgba(LevelIndex::from_u32(0), TileCoord::new(0, 0))
        .unwrap();
    assert_eq!((tile.width, tile.height), (2, 2));
    assert_eq!(tile.rgba.len(), 2 * 2 * 4);
    assert_eq!(&tile.rgba[0..4], &[255, 0, 0, 255]);
}

#[test]
fn whole_level_layout_uses_viewer_display_tiles() {
    let layout = LevelTileLayout::WholeLevel {
        width: 12960,
        height: 9472,
        virtual_tile_width: 480,
        virtual_tile_height: 8,
    };

    assert_eq!(
        layout.display_tile_size(),
        (DEFAULT_DISPLAY_TILE_SIZE, DEFAULT_DISPLAY_TILE_SIZE)
    );
    assert_eq!(layout.grid_size(), Some((26, 19)));
    assert!(layout.contains(TileCoord::new(25, 18)));
    assert!(!layout.contains(TileCoord::new(26, 18)));
}

#[test]
#[ignore = "requires DICOM_VIEWER_WSI_FIXTURE to point at a local WSI file"]
fn opens_local_wsi_fixture_from_env() {
    let path = std::env::var_os("DICOM_VIEWER_WSI_FIXTURE")
        .map(PathBuf::from)
        .expect("DICOM_VIEWER_WSI_FIXTURE must point at a local WSI file");
    let study = ViewerStudy::open_path(&path).unwrap();
    let summary = study.summary();
    assert!(!summary.levels.is_empty());
    let overview = summary
        .levels
        .iter()
        .min_by_key(|level| u128::from(level.width) * u128::from(level.height))
        .expect("summary has levels");
    let tile = study
        .read_tile_rgba(overview.index, TileCoord::new(0, 0))
        .unwrap();
    assert!(tile.width > 0);
    assert!(tile.height > 0);
    assert_eq!(
        tile.rgba.len(),
        tile.width as usize * tile.height as usize * 4
    );
}

fn write_test_dicom(path: &Path, sop_instance_uid: &'static str, series_uid: &'static str) {
    let mut object = InMemDicomObject::new_empty();
    object.put(DataElement::new(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE,
    ));
    object.put(DataElement::new(
        tags::SOP_INSTANCE_UID,
        VR::UI,
        sop_instance_uid,
    ));
    object.put(DataElement::new(
        tags::SERIES_INSTANCE_UID,
        VR::UI,
        series_uid,
    ));
    object.put(DataElement::new(
        tags::IMAGE_TYPE,
        VR::CS,
        "ORIGINAL\\PRIMARY\\VOLUME\\NONE",
    ));
    object.put(DataElement::new(
        tags::ROWS,
        VR::US,
        PrimitiveValue::from(2u16),
    ));
    object.put(DataElement::new(
        tags::COLUMNS,
        VR::US,
        PrimitiveValue::from(2u16),
    ));
    object.put(DataElement::new(
        tags::TOTAL_PIXEL_MATRIX_ROWS,
        VR::UL,
        PrimitiveValue::from(2u32),
    ));
    object.put(DataElement::new(
        tags::TOTAL_PIXEL_MATRIX_COLUMNS,
        VR::UL,
        PrimitiveValue::from(2u32),
    ));
    object.put(DataElement::new(
        tags::NUMBER_OF_FRAMES,
        VR::IS,
        PrimitiveValue::from(1u32),
    ));
    object.put(DataElement::new(
        tags::DIMENSION_ORGANIZATION_TYPE,
        VR::CS,
        "TILED_FULL",
    ));
    object.put(DataElement::new(
        tags::SAMPLES_PER_PIXEL,
        VR::US,
        PrimitiveValue::from(3u16),
    ));
    object.put(DataElement::new(
        tags::PHOTOMETRIC_INTERPRETATION,
        VR::CS,
        "RGB",
    ));
    object.put(DataElement::new(
        tags::PLANAR_CONFIGURATION,
        VR::US,
        PrimitiveValue::from(0u16),
    ));
    object.put(DataElement::new(
        tags::BITS_ALLOCATED,
        VR::US,
        PrimitiveValue::from(8u16),
    ));
    object.put(DataElement::new(
        tags::BITS_STORED,
        VR::US,
        PrimitiveValue::from(8u16),
    ));
    object.put(DataElement::new(
        tags::HIGH_BIT,
        VR::US,
        PrimitiveValue::from(7u16),
    ));
    object.put(DataElement::new(
        tags::PIXEL_REPRESENTATION,
        VR::US,
        PrimitiveValue::from(0u16),
    ));
    object.put(DataElement::new(
        tags::PIXEL_SPACING,
        VR::DS,
        "0.00025\\0.00025",
    ));
    object.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PrimitiveValue::from(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]),
    ));
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .media_storage_sop_class_uid(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
                .media_storage_sop_instance_uid(sop_instance_uid)
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN),
        )
        .unwrap()
        .write_to_file(path)
        .unwrap();
}

#[cfg(target_os = "macos")]
fn write_test_htj2k_dicom(path: &Path, codestream: Vec<u8>) {
    let sop_instance_uid = "1.2.826.0.1.3680043.10.777.2001";
    let series_uid = "1.2.826.0.1.3680043.10.777.2000";
    let mut object = InMemDicomObject::new_empty();
    for element in [
        DataElement::new(
            tags::SOP_CLASS_UID,
            VR::UI,
            PrimitiveValue::from(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE),
        ),
        DataElement::new(
            tags::SOP_INSTANCE_UID,
            VR::UI,
            PrimitiveValue::from(sop_instance_uid),
        ),
        DataElement::new(
            tags::SERIES_INSTANCE_UID,
            VR::UI,
            PrimitiveValue::from(series_uid),
        ),
        DataElement::new(
            tags::IMAGE_TYPE,
            VR::CS,
            PrimitiveValue::from("ORIGINAL\\PRIMARY\\VOLUME\\NONE"),
        ),
        DataElement::new(tags::ROWS, VR::US, PrimitiveValue::from(2_u16)),
        DataElement::new(tags::COLUMNS, VR::US, PrimitiveValue::from(2_u16)),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_ROWS,
            VR::UL,
            PrimitiveValue::from(2_u32),
        ),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_COLUMNS,
            VR::UL,
            PrimitiveValue::from(16_u32),
        ),
        DataElement::new(tags::NUMBER_OF_FRAMES, VR::IS, PrimitiveValue::from(8_u32)),
        DataElement::new(
            tags::DIMENSION_ORGANIZATION_TYPE,
            VR::CS,
            PrimitiveValue::from("TILED_FULL"),
        ),
        DataElement::new(tags::SAMPLES_PER_PIXEL, VR::US, PrimitiveValue::from(3_u16)),
        DataElement::new(
            tags::PHOTOMETRIC_INTERPRETATION,
            VR::CS,
            PrimitiveValue::from("RGB"),
        ),
        DataElement::new(
            tags::PLANAR_CONFIGURATION,
            VR::US,
            PrimitiveValue::from(0_u16),
        ),
        DataElement::new(tags::BITS_ALLOCATED, VR::US, PrimitiveValue::from(8_u16)),
        DataElement::new(tags::BITS_STORED, VR::US, PrimitiveValue::from(8_u16)),
        DataElement::new(tags::HIGH_BIT, VR::US, PrimitiveValue::from(7_u16)),
        DataElement::new(
            tags::PIXEL_REPRESENTATION,
            VR::US,
            PrimitiveValue::from(0_u16),
        ),
    ] {
        object.put(element);
    }
    let pixel_sequence = PixelFragmentSequence::from(
        (0..8)
            .map(|_| Fragments::new(codestream.clone(), 0))
            .collect::<Vec<_>>(),
    );
    object.put(DataElement::<InMemDicomObject>::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::from(pixel_sequence),
    ));
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .media_storage_sop_class_uid(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
                .media_storage_sop_instance_uid(sop_instance_uid)
                .transfer_syntax("1.2.840.10008.1.2.4.201"),
        )
        .unwrap()
        .write_to_file(path)
        .unwrap();
}
