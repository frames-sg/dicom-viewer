use super::canvas::{fallback_levels, PREFETCH_MARGIN_TILES};
use super::measurement::{format_measurement_distance, measurement_distance, MeasurementDistance};
use super::viewport::choose_display_level_index;
use super::*;
use dicom_viewer_core::LevelTileLayout;

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

fn summary() -> StudySummary {
    StudySummary {
        source_path: PathBuf::from("slide.ndpi"),
        source_kind: SourceKind::File,
        format_label: "Hamamatsu WSI".into(),
        tile_decode_backend: TileDecodeBackend::Cpu,
        file_count: 1,
        dicom_instance_count: 0,
        levels: vec![
            level(0, 4096, 4096, 1.0),
            level(1, 1024, 1024, 4.0),
            level(2, 256, 256, 16.0),
        ],
        instances: Vec::new(),
        warnings: Vec::new(),
        mpp: None,
        objective_power: None,
    }
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
        width: 2048,
        height: 2048,
        downsample: 32.0,
        tile_layout: LevelTileLayout::Regular {
            tile_width: 2048,
            tile_height: 2048,
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
fn wheel_zoom_direction_is_inverted_for_natural_scroll() {
    assert!(wheel_zoom_factor(120.0) < 1.0);
    assert!(wheel_zoom_factor(-120.0) > 1.0);
    assert_eq!(wheel_zoom_factor(0.0), 1.0);
}

#[test]
fn measurement_distance_uses_mpp_axes() {
    let mut summary = summary();
    summary.mpp = Some((0.25, 0.5));

    let distance = measurement_distance(&summary, vec2(10.0, 20.0), vec2(26.0, 26.0));

    assert_eq!(distance, MeasurementDistance::Microns(4.0_f64.hypot(3.0)));
    assert_eq!(format_measurement_distance(distance), "5.00 \u{00B5}m");
}

#[test]
fn measurement_distance_falls_back_to_base_pixels_without_mpp() {
    let summary = summary();

    let distance = measurement_distance(&summary, vec2(10.0, 20.0), vec2(13.0, 24.0));

    assert_eq!(distance, MeasurementDistance::BasePixels(5.0));
    assert_eq!(format_measurement_distance(distance), "5.00 px");
}

#[test]
fn measurement_places_two_points_and_clear_keeps_tool_active() {
    let mut measurement = MeasurementState {
        active: true,
        ..MeasurementState::default()
    };

    measurement.place_next_point(vec2(1.0, 2.0));
    measurement.place_next_point(vec2(3.0, 4.0));
    measurement.place_next_point(vec2(5.0, 6.0));

    assert_eq!(
        measurement.points,
        [Some(vec2(1.0, 2.0)), Some(vec2(3.0, 4.0))]
    );
    assert!(measurement.has_points());

    measurement.clear_points();

    assert!(measurement.active);
    assert_eq!(measurement.points, [None, None]);
    assert!(!measurement.has_points());
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
