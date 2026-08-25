use super::canvas::{fallback_levels, PREFETCH_MARGIN_TILES};
use super::viewport::{
    base_size, choose_display_level_index, choose_render_level_with_hysteresis,
    nearest_grid_coordinates, tile_screen_rect, CanvasView,
};
use super::*;
use dicom_viewer_core::{LevelTileLayout, TileCoord};
use eframe::egui::Vec2;

fn level(index: usize, width: u64, height: u64, downsample: f64) -> LevelInfo {
    LevelInfo {
        index: LevelIndex::from_usize(index).expect("test level index should fit in u32"),
        width,
        height,
        downsample,
        tile_layout: LevelTileLayout::Regular {
            tile_width: 256,
            tile_height: 256,
            tiles_across: width.div_ceil(256),
            tiles_down: height.div_ceil(256),
        },
    }
}

pub(super) fn summary() -> StudySummary {
    StudySummary {
        source_path: PathBuf::from("slide.ndpi"),
        source_kind: SourceKind::File,
        format_label: "Hamamatsu WSI".into(),
        tile_decode_backend: TileDecodeBackend::Cpu,
        file_count: 1,
        dicom_instance_count: 0,
        canvas_dimensions: (4096, 4096),
        levels: vec![
            level(0, 4096, 4096, 1.0),
            level(1, 1024, 1024, 4.0),
            level(2, 256, 256, 16.0),
        ],
        instances: Vec::new(),
        warnings: Vec::new(),
        mpp: None,
        objective_power: None,
        color_management: dicom_viewer_core::ColorManagementSummary::unprofiled(),
    }
}

pub(super) fn run_ui(mut render: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
    let context = egui::Context::default();
    context.run_ui(egui::RawInput::default(), |ui| render(ui))
}

pub(super) fn wait_for_background<T>(label: &str, mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(result) = poll() {
            return result;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::yield_now();
    }
}

pub(super) fn write_source_wsi(path: &std::path::Path) {
    use dicom_core::value::{DataSetSequence, PrimitiveValue, Value};
    use dicom_core::{DataElement, Length, VR};
    use dicom_dictionary_std::{tags, uids};
    use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

    const SOP_UID: &str = "1.2.826.0.1.3680043.10.777.9901";
    let mut origin = InMemDicomObject::new_empty();
    origin.put(DataElement::new(
        tags::X_OFFSET_IN_SLIDE_COORDINATE_SYSTEM,
        VR::DS,
        "0",
    ));
    origin.put(DataElement::new(
        tags::Y_OFFSET_IN_SLIDE_COORDINATE_SYSTEM,
        VR::DS,
        "0",
    ));
    let mut object = InMemDicomObject::new_empty();
    for element in [
        DataElement::new(
            tags::SOP_CLASS_UID,
            VR::UI,
            uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE,
        ),
        DataElement::new(tags::SOP_INSTANCE_UID, VR::UI, SOP_UID),
        DataElement::new(tags::STUDY_INSTANCE_UID, VR::UI, "2.25.9902"),
        DataElement::new(tags::SERIES_INSTANCE_UID, VR::UI, "2.25.9903"),
        DataElement::new(tags::FRAME_OF_REFERENCE_UID, VR::UI, "2.25.9904"),
        DataElement::new(tags::PATIENT_NAME, VR::PN, "Research^Slide"),
        DataElement::new(tags::PATIENT_ID, VR::LO, "R-1"),
        DataElement::new(tags::STUDY_DATE, VR::DA, "20260814"),
        DataElement::new(tags::STUDY_TIME, VR::TM, "120000"),
        DataElement::new(tags::STUDY_ID, VR::SH, "STUDY-1"),
        DataElement::new(tags::ACCESSION_NUMBER, VR::SH, ""),
        DataElement::new(tags::ROWS, VR::US, PrimitiveValue::from(4_u16)),
        DataElement::new(tags::COLUMNS, VR::US, PrimitiveValue::from(4_u16)),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_ROWS,
            VR::UL,
            PrimitiveValue::from(8_u32),
        ),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_COLUMNS,
            VR::UL,
            PrimitiveValue::from(8_u32),
        ),
        DataElement::new(tags::IMAGE_ORIENTATION_SLIDE, VR::DS, "1\\0\\0\\0\\1\\0"),
        DataElement::new(tags::PIXEL_SPACING, VR::DS, "0.00025\\0.00025"),
        DataElement::new(tags::SLICE_THICKNESS, VR::DS, "0.001"),
    ] {
        object.put(element);
    }
    object.put(DataElement::new(
        tags::TOTAL_PIXEL_MATRIX_ORIGIN_SEQUENCE,
        VR::SQ,
        Value::from(DataSetSequence::new(vec![origin], Length::UNDEFINED)),
    ));
    object.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PrimitiveValue::from(vec![0_u8; 48]),
    ));
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .media_storage_sop_class_uid(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
                .media_storage_sop_instance_uid(SOP_UID)
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN),
        )
        .unwrap()
        .write_to_file(path)
        .unwrap();
}

fn headless_app(initial_path: Option<PathBuf>) -> Option<(DicomViewerApp, egui::Context)> {
    let render_state = tile::render_state()?;
    let context = egui::Context::default();
    let mut creation = eframe::CreationContext::_new_kittest(context.clone());
    creation.wgpu_render_state = Some(render_state);
    Some((DicomViewerApp::new(&creation, initial_path), context))
}

#[test]
fn headless_app_runs_empty_logic_and_ui_with_real_renderer_state() {
    let Some((mut app, context)) = headless_app(None) else {
        return;
    };
    let mut frame = eframe::Frame::_new_kittest();

    let output = context.run_ui(egui::RawInput::default(), |ui| {
        eframe::App::logic(&mut app, ui.ctx(), &mut frame);
        eframe::App::ui(&mut app, ui, &mut frame);
    });
    assert!(!output.shapes.is_empty());
    assert!(app.study.is_none());
    assert_eq!(app.active_generation, 0);

    app.show_facts_panel = true;
    let output = context.run_ui(egui::RawInput::default(), |ui| {
        eframe::App::ui(&mut app, ui, &mut frame);
    });
    assert!(!output.shapes.is_empty());
    assert!(app.show_facts_panel);
}

#[test]
fn headless_app_reports_no_study_actions_and_failed_open_without_panics() {
    let Some((mut app, context)) = headless_app(None) else {
        return;
    };

    app.start_annotation_load(PathBuf::from("sidecar.dcm"), None, &context);
    assert!(app.status.contains("require an open VL WSI"));
    app.pick_profiled_geojson(&context);
    assert!(app.status.contains("requires an open VL WSI"));
    app.pick_structured_report(&context, false);
    assert!(app.status.contains("require an open VL WSI"));
    app.pick_profiled_raster(&context);
    assert!(app.status.contains("requires an open VL WSI"));
    app.save_dicom_ann(&context);
    assert!(app.status.contains("requires a VL WSI"));
    app.export_dicom_seg(&context);
    assert!(app.status.contains("requires a VL WSI"));

    let missing = PathBuf::from("definitely-missing-headless-viewer-input.svs");
    app.start_open_path(missing.clone(), &context);
    assert!(app.status.starts_with("Opening"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.status.starts_with("Opening") && Instant::now() < deadline {
        app.poll_open_job(&context);
        std::thread::yield_now();
    }
    assert!(app.status.starts_with("Failed to open"), "{}", app.status);
    assert!(app.study.is_none());
    app.poll_annotation_jobs(&context);
}

#[test]
fn zoom_selects_closest_pyramid_level() {
    let summary = summary();
    assert_eq!(
        choose_render_level(&summary, 1.0).unwrap().index,
        LevelIndex::from_u32(0)
    );
    assert_eq!(
        choose_render_level(&summary, 0.25).unwrap().index,
        LevelIndex::from_u32(1)
    );
    assert_eq!(
        choose_render_level(&summary, 0.0625).unwrap().index,
        LevelIndex::from_u32(2)
    );
}

#[test]
fn render_level_does_not_upscale_lower_resolution_tiles() {
    let summary = summary();
    assert_eq!(
        choose_render_level(&summary, 0.40).unwrap().index,
        LevelIndex::from_u32(0)
    );
    assert_eq!(
        choose_render_level(&summary, 0.25).unwrap().index,
        LevelIndex::from_u32(1)
    );
    assert_eq!(
        choose_render_level(&summary, 0.10).unwrap().index,
        LevelIndex::from_u32(1)
    );
    assert_eq!(
        choose_render_level(&summary, 0.0625).unwrap().index,
        LevelIndex::from_u32(2)
    );
}

#[test]
fn render_level_hysteresis_holds_across_small_threshold_oscillations() {
    let summary = summary();
    let fine = LevelIndex::from_u32(0);
    let coarse = LevelIndex::from_u32(1);

    assert_eq!(
        choose_render_level_with_hysteresis(&summary, 0.24, Some(fine))
            .unwrap()
            .index,
        fine
    );
    assert_eq!(
        choose_render_level_with_hysteresis(&summary, 0.22, Some(fine))
            .unwrap()
            .index,
        coarse
    );
    assert_eq!(
        choose_render_level_with_hysteresis(&summary, 0.26, Some(coarse))
            .unwrap()
            .index,
        coarse
    );
    assert_eq!(
        choose_render_level_with_hysteresis(&summary, 0.28, Some(coarse))
            .unwrap()
            .index,
        fine
    );
}

#[test]
fn display_level_holds_ready_level_until_target_is_ready() {
    assert_eq!(
        choose_display_level_index(Some(LevelIndex::from_u32(1)), LevelIndex::from_u32(0), 1, 0,),
        LevelIndex::from_u32(1)
    );
    assert_eq!(
        choose_display_level_index(Some(LevelIndex::from_u32(1)), LevelIndex::from_u32(0), 0, 0,),
        LevelIndex::from_u32(0)
    );
}

#[test]
fn every_pyramid_level_is_reachable_by_zoom() {
    // Each level in a power-of-two pyramid must win selection for some
    // zoom, otherwise zooming would skip past it and it would "never
    // display". Sweep a wide zoom range and confirm full coverage.
    let summary = summary();
    let mut reached = std::collections::HashSet::new();
    let mut zoom = MIN_ZOOM;
    while zoom <= MAX_ZOOM {
        if let Some(level) = choose_render_level(&summary, zoom) {
            reached.insert(level.index);
        }
        zoom *= 1.05;
    }
    for level in &summary.levels {
        assert!(
            reached.contains(&level.index),
            "level {} is never selected across the zoom range",
            level.index
        );
    }
}

#[test]
fn visible_tiles_cover_view_and_margin() {
    let summary = summary();
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
    let tiles = visible_tiles(rect, &summary.levels[0], 7, vec2(512.0, 512.0), 1.0, 0);
    assert_eq!(tiles.len(), 4);
    assert!(tiles
        .iter()
        .any(|tile| tile.key.coord.col() == 1 && tile.key.coord.row() == 1));

    let with_margin = visible_tiles(
        rect,
        &summary.levels[0],
        7,
        vec2(512.0, 512.0),
        1.0,
        PREFETCH_MARGIN_TILES,
    );
    assert!(with_margin.len() > tiles.len());
}

#[test]
fn visible_tile_planning_is_bounded_for_pathological_grids() {
    let mut summary = summary();
    summary.levels = vec![LevelInfo {
        index: LevelIndex::from_u32(0),
        width: 25_600,
        height: 25_600,
        downsample: 1.0,
        tile_layout: LevelTileLayout::Regular {
            tile_width: 256,
            tile_height: 256,
            tiles_across: 100,
            tiles_down: 100,
        },
    }];
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(25_600.0, 25_600.0));

    let tiles = visible_tiles(
        rect,
        &summary.levels[0],
        1,
        vec2(12_800.0, 12_800.0),
        1.0,
        0,
    );

    assert_eq!(
        tiles.len(),
        8_193,
        "visible planning must retain one overflow candidate so the atomic scheduler cap can diagnose and trim visible-only overflow"
    );
    assert!(
        tiles
            .iter()
            .any(|tile| tile.key.coord.col() == 50 && tile.key.coord.row() == 4),
        "bounded planning must retain globally nearer axis tiles instead of farther corners from an arbitrary rectangular crop"
    );
}

#[test]
fn nearest_grid_planning_is_bounded_and_center_first_for_huge_grids() {
    let cols = 1_000_000_000;
    let rows = 2_000_000_000;

    let coordinates = nearest_grid_coordinates(cols, rows, 8_192);

    assert_eq!(coordinates.len(), 8_192);
    assert_eq!(coordinates[0], (0, rows / 2, cols / 2));
    assert!(coordinates.windows(2).all(|pair| pair[0] <= pair[1]));
}

#[test]
fn canonical_canvas_drives_fit_tile_geometry_measurements_and_edge_reachability() {
    let mut summary = summary();
    summary.canvas_dimensions = (1005, 781);
    summary.levels = vec![LevelInfo {
        index: LevelIndex::from_u32(1),
        width: 251,
        height: 195,
        downsample: 4.0,
        tile_layout: LevelTileLayout::Regular {
            tile_width: 128,
            tile_height: 128,
            tiles_across: 2,
            tiles_down: 2,
        },
    }];

    assert_eq!(base_size(&summary), Some(vec2(1005.0, 781.0)));

    let fit_rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1005.0, 781.0));
    let mut camera = CameraState::default();
    camera.reset_for_study(&summary);
    camera.prepare_canvas(fit_rect, &summary);
    assert_eq!(camera.target_view().center_base, vec2(502.5, 390.5));
    assert!((camera.target_view().zoom - 1.0).abs() < f32::EPSILON);

    let edge_rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
    let edge_tiles = visible_tiles(edge_rect, &summary.levels[0], 3, vec2(955.0, 731.0), 1.0, 0);
    assert!(edge_tiles
        .iter()
        .any(|tile| tile.key.coord == TileCoord::new(1, 1)));

    let tile_rect = tile_screen_rect(
        CanvasView {
            rect: edge_rect,
            center_base: Vec2::ZERO,
            zoom: 1.0,
        },
        &summary.levels[0],
        TileCoord::new(1, 1),
        123,
        67,
    );
    assert_eq!(tile_rect.min, pos2(562.0, 562.0));
    assert_eq!(tile_rect.size(), vec2(492.0, 268.0));
}

#[test]
fn fallback_levels_include_coarser_levels_and_held_level() {
    let summary = summary();
    let levels = fallback_levels(&summary, &summary.levels[1], Some(&summary.levels[0]));
    let indexes = levels.iter().map(|level| level.index).collect::<Vec<_>>();

    assert_eq!(
        indexes,
        vec![LevelIndex::from_u32(2), LevelIndex::from_u32(0)]
    );
}

#[test]
fn fallback_levels_include_regular_1024_tiles_for_clean_failure_recovery() {
    let mut summary = summary();
    summary.levels.push(LevelInfo {
        index: LevelIndex::from_u32(3),
        width: 1024,
        height: 1024,
        downsample: 32.0,
        tile_layout: LevelTileLayout::Regular {
            tile_width: 1024,
            tile_height: 1024,
            tiles_across: 1,
            tiles_down: 1,
        },
    });

    let levels = fallback_levels(&summary, &summary.levels[1], None);
    let indexes = levels.iter().map(|level| level.index).collect::<Vec<_>>();

    assert_eq!(
        indexes,
        vec![LevelIndex::from_u32(3), LevelIndex::from_u32(2)],
        "a failed target must retain an ordinary 1024x1024 coarser layer to draw underneath it"
    );
}

#[test]
fn fallback_levels_skip_target_and_irregular_levels() {
    let mut summary = summary();
    summary.levels.push(LevelInfo {
        index: LevelIndex::from_u32(3),
        width: 128,
        height: 128,
        downsample: 32.0,
        tile_layout: LevelTileLayout::Irregular {
            tile_advance: (64.0, 64.0),
            tile_count: 4,
        },
    });

    let levels = fallback_levels(&summary, &summary.levels[1], None);
    let indexes = levels.iter().map(|level| level.index).collect::<Vec<_>>();

    assert_eq!(indexes, vec![LevelIndex::from_u32(2)]);
}

#[test]
fn fallback_levels_skip_oversized_coarser_tiles() {
    let mut summary = summary();
    summary.levels.push(LevelInfo {
        index: LevelIndex::from_u32(3),
        width: 4096,
        height: 4096,
        downsample: 32.0,
        tile_layout: LevelTileLayout::Regular {
            tile_width: 4096,
            tile_height: 4096,
            tiles_across: 1,
            tiles_down: 1,
        },
    });

    let levels = fallback_levels(&summary, &summary.levels[1], None);
    let indexes = levels.iter().map(|level| level.index).collect::<Vec<_>>();

    assert_eq!(indexes, vec![LevelIndex::from_u32(2)]);
}

#[test]
fn frame_stats_tracks_display_cadence() {
    let mut stats = FrameStats::default();
    stats.record(1.0 / 120.0, 1.0 / 60.0);
    stats.record(1.0 / 120.0, 1.0 / 60.0);

    let info = stats.info().expect("fps should be recorded");
    assert!((info.fps - 120.0).abs() < 0.01, "fps={}", info.fps);
    assert!(
        info.display_fps >= 119.0,
        "display_fps={}",
        info.display_fps
    );
}

#[test]
fn frame_stats_tracks_60hz_without_penalty() {
    let mut stats = FrameStats::default();
    stats.record(1.0 / 60.0, 1.0 / 60.0);
    stats.record(1.0 / 60.0, 1.0 / 60.0);

    let info = stats.info().expect("fps should be recorded");
    assert!((info.fps - 60.0).abs() < 0.01, "fps={}", info.fps);
    assert_eq!(fps_color(info), theme::GREEN);
}

#[test]
fn camera_motion_interpolates_toward_target() {
    let mut motion = CameraMotion::default();
    let start = CameraView {
        center_base: vec2(0.0, 0.0),
        zoom: 1.0,
    };
    let target = CameraView {
        center_base: vec2(100.0, 50.0),
        zoom: 4.0,
    };
    motion.reset(start);

    let (view, animating) = motion.render_view(target, 1.0 / 60.0);

    assert!(animating);
    assert!(view.center_base.x > start.center_base.x);
    assert!(view.center_base.x < target.center_base.x);
    assert!(view.center_base.y > start.center_base.y);
    assert!(view.center_base.y < target.center_base.y);
    assert!(view.zoom > start.zoom);
    assert!(view.zoom < target.zoom);
}

#[test]
fn disabled_camera_motion_uses_target_view_immediately() {
    let mut motion = CameraMotion {
        enabled: false,
        ..CameraMotion::default()
    };
    motion.reset(CameraView {
        center_base: vec2(0.0, 0.0),
        zoom: 1.0,
    });
    let target = CameraView {
        center_base: vec2(100.0, 50.0),
        zoom: 4.0,
    };

    let (view, animating) = motion.render_view(target, 1.0 / 60.0);

    assert!(!animating);
    assert_eq!(view.center_base, target.center_base);
    assert_eq!(view.zoom, target.zoom);
}

#[test]
fn camera_state_owns_fit_resize_pan_and_zoom_lifecycle() {
    let summary = summary();
    let mut camera = CameraState::default();
    let small = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
    let large = Rect::from_min_size(pos2(0.0, 0.0), vec2(1024.0, 1024.0));
    camera.reset_for_study(&summary);

    camera.prepare_canvas(small, &summary);
    assert_eq!(camera.target_view().center_base, vec2(2048.0, 2048.0));
    assert!((camera.target_view().zoom - 0.125).abs() < f32::EPSILON);

    camera.prepare_canvas(large, &summary);
    assert!((camera.target_view().zoom - 0.25).abs() < f32::EPSILON);

    camera.pan_by(vec2(100.0, 0.0));
    let panned = camera.target_view();
    camera.prepare_canvas(small, &summary);
    assert_eq!(camera.target_view().zoom, panned.zoom);
    assert_ne!(camera.target_view().center_base, vec2(2048.0, 2048.0));

    camera.zoom_about_center(small, 2.0);
    assert!((camera.target_view().zoom - 0.5).abs() < f32::EPSILON);
}

#[test]
fn camera_zoom_out_stops_ten_percent_past_fit() {
    let summary = summary();
    let mut camera = CameraState::default();
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
    camera.reset_for_study(&summary);
    camera.prepare_canvas(rect, &summary);

    camera.zoom_about_center(rect, 0.01);

    let fit_zoom = 512.0 / 4096.0;
    assert!((camera.target_view().zoom - fit_zoom * 0.9).abs() < f32::EPSILON);
}

#[test]
fn camera_zoom_out_at_slide_edge_preserves_center_focus() {
    let summary = summary();
    let mut camera = CameraState::default();
    *camera.smoothing_enabled_mut() = false;
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
    camera.reset_for_study(&summary);
    camera.prepare_canvas(rect, &summary);
    camera.zoom_about_center(rect, 8.0);
    camera.prepare_canvas(rect, &summary);

    camera.pan_by(vec2(10_000.0, 10_000.0));
    camera.prepare_canvas(rect, &summary);
    let focus = camera
        .frame(rect, &summary, 1.0 / 60.0)
        .rendered
        .center_base;
    assert_eq!(focus, Vec2::ZERO);

    camera.zoom_about_center(rect, 0.5);
    camera.prepare_canvas(rect, &summary);
    assert_eq!(
        camera
            .frame(rect, &summary, 1.0 / 60.0)
            .rendered
            .center_base,
        focus
    );

    camera.zoom_about_center(rect, 2.0);
    camera.prepare_canvas(rect, &summary);
    assert_eq!(
        camera
            .frame(rect, &summary, 1.0 / 60.0)
            .rendered
            .center_base,
        focus
    );
}

#[test]
fn pointer_zoom_preserves_the_base_point_in_the_rendered_frame() {
    let summary = summary();
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
    let pointer = pos2(400.0, 180.0);
    let mut camera = CameraState::default();
    camera.reset_for_study(&summary);
    camera.prepare_canvas(rect, &summary);
    let _ = camera.frame(rect, &summary, 1.0 / 60.0);
    camera.zoom_about_center(rect, 4.0);
    let frame = camera.frame(rect, &summary, 1.0 / 60.0);
    assert!(frame.animating);
    assert_ne!(frame.rendered.zoom, frame.target.zoom);
    let displayed_base = screen_to_base(
        rect,
        pointer,
        frame.rendered.center_base,
        frame.rendered.zoom,
    );

    camera.zoom_around_rendered(rect, pointer, 1.25, frame.rendered);

    let target = camera.target_view();
    let target_base = screen_to_base(rect, pointer, target.center_base, target.zoom);
    assert!((target_base - displayed_base).length() < 0.001);
}

#[test]
fn wheel_zoom_direction_is_inverted_for_natural_scroll() {
    assert!(wheel_zoom_factor(120.0) < 1.0);
    assert!(wheel_zoom_factor(-120.0) > 1.0);
    assert_eq!(wheel_zoom_factor(0.0), 1.0);
}

#[test]
fn fps_color_is_relative_to_estimated_display_rate() {
    assert_eq!(
        fps_color(FrameRateInfo {
            fps: 108.0,
            display_fps: 120.0,
        }),
        theme::GREEN
    );
    assert_eq!(
        fps_color(FrameRateInfo {
            fps: 95.0,
            display_fps: 120.0,
        }),
        theme::WARN
    );
    assert_eq!(
        fps_color(FrameRateInfo {
            fps: 70.0,
            display_fps: 120.0,
        }),
        theme::TEXT_MUTED
    );
}
