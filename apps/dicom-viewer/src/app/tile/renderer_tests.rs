use super::*;

#[test]
fn decoded_cpu_tiles_and_idle_renderer_surface_report_exact_state() {
    let rgba = dicom_viewer_core::RgbaTile {
        width: 2,
        height: 1,
        rgba: vec![1, 2, 3, 255, 4, 5, 6, 255],
    };
    let decoded = DecodedTile::from_render_tile(RenderTile::Cpu(rgba.clone())).unwrap();
    assert!(decoded.is_cpu());
    assert_eq!(decoded.dimensions(), (2, 1));
    assert_eq!(
        decoded.memory_cost().unwrap(),
        TileFootprint::for_cpu_rgba(2, 1, 8).unwrap()
    );
    let invalid = DecodedTile::from_rgba_tile(dicom_viewer_core::RgbaTile {
        width: 2,
        height: 1,
        rgba: vec![0; 7],
    });
    assert!(invalid.memory_cost().unwrap_err().contains("require 8"));

    let Some(state) = render_state() else {
        return;
    };
    let mut renderer = TileRenderer::new(state, 4096);
    assert!(renderer.viewer_open_options().is_ok());
    let _ = renderer.backend_warning();
    renderer.set_cache_protection(HashSet::new(), HashSet::new());
    renderer.set_interactive(true, false);
    renderer.record_app_ui_cpu_time(Duration::from_micros(5));
    renderer.record_zoom_input();
    renderer.record_level_preparation(Duration::from_micros(7), LevelPreparationStatus::Prepared);
    renderer.record_dicom_index_diagnostics(DicomIndexDiagnosticSource::Preparation, &[]);
    renderer.observe_interaction_target(LevelIndex::from_u32(0), TileCoverage::default());
    assert_eq!(renderer.loading_count(), 0);
    assert!(renderer.tile_failure().is_none());
    assert!(renderer.cpu_fallback().is_none());
    assert_eq!(
        renderer.uncovered_tile_count(&[], LevelIndex::from_u32(0)),
        0
    );
    assert_eq!(
        renderer.coverage(&[], LevelIndex::from_u32(0)),
        TileCoverage::default()
    );

    let relevant = HashSet::new();
    let request = TilePollRequest {
        generation: 1,
        relevant_tiles: &relevant,
        visible_tiles: &[],
        transition_target_tiles: &[],
        fallback_tiles: &[],
        overview_tiles: &[],
        background_prefetch_tiles: &[],
        render_level_index: LevelIndex::from_u32(0),
        interactive: false,
    };
    let context = egui::Context::default();
    renderer.request_debug_stats_repaint(&context);
    renderer.poll_results(&context, request);
    let _ = renderer.debug_stats_enabled();
    let _ = renderer.debug_stats_text();
    renderer.clear();
}
