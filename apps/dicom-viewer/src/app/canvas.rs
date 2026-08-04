use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use dicom_viewer_core::{
    DicomIndexDiagnostic, LevelIndex, LevelInfo, StudySummary, TileDecodeBackend,
    ViewerCacheBudgets, ViewerOpenOptions, ViewerStudy,
};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Rect, Stroke, StrokeKind};

use super::camera::CameraFrame;
#[cfg(test)]
use super::camera::CameraView;
use super::level_warmer::{LevelWarmer, LevelWarmerEvent};
use super::theme;
use super::tile::{
    DicomIndexDiagnosticSource, FrameTileDemand, LevelPreparationStatus, TileFailureInfo,
    TilePollRequest, TileRenderer, VisibleTile,
};
#[cfg(test)]
use super::viewport::visible_tiles;
use super::viewport::{
    base_size, choose_display_level_index, choose_render_level,
    choose_render_level_with_hysteresis, level_by_index, nearest_grid_coordinates,
    visible_tiles_with_limit,
};

pub(super) const PREFETCH_MARGIN_TILES: i64 = 1;
const OVERVIEW_PIN_BYTES: usize = 32 * 1024 * 1024;
const MAX_FALLBACK_TILE_BYTES: u64 = OVERVIEW_PIN_BYTES as u64;
const MAX_PLANNED_TILES: usize = 8_192;
const MAX_FRAME_PLANNING_REFERENCES: usize = MAX_PLANNED_TILES + 1;
pub(super) const TILE_RESIDENT_CACHE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug)]
struct OverviewTileCache {
    generation: Option<u64>,
    tiles: Arc<[VisibleTile]>,
}

impl Default for OverviewTileCache {
    fn default() -> Self {
        Self {
            generation: None,
            tiles: Arc::from(Vec::new()),
        }
    }
}

impl OverviewTileCache {
    fn tiles_for(&mut self, summary: &StudySummary, generation: u64) -> Arc<[VisibleTile]> {
        if self.generation != Some(generation) {
            self.tiles = Arc::from(overview_tiles(summary, generation));
            self.generation = Some(generation);
        }
        Arc::clone(&self.tiles)
    }

    fn clear(&mut self) {
        self.generation = None;
        self.tiles = Arc::from(Vec::new());
    }
}

pub(super) struct SlideCanvas {
    tiles: TileRenderer,
    level_warmer: Option<LevelWarmer>,
    level_warmer_error: Option<String>,
    last_level_warmer_event: Option<String>,
    displayed_level: Option<LevelIndex>,
    render_level_hint: Option<LevelIndex>,
    overview_cache: OverviewTileCache,
}

impl SlideCanvas {
    pub(super) fn new(render_state: eframe::egui_wgpu::RenderState) -> Self {
        let resident_bytes = ViewerCacheBudgets::from_environment()
            .unwrap_or_default()
            .viewer_tile_bytes;
        let resident_bytes = usize::try_from(resident_bytes).unwrap_or(TILE_RESIDENT_CACHE_BYTES);
        let (level_warmer, level_warmer_error) = match LevelWarmer::new() {
            Ok(warmer) => (Some(warmer), None),
            Err(error) => (None, Some(format!("level warmer failed to start: {error}"))),
        };
        Self {
            tiles: TileRenderer::new(render_state, resident_bytes),
            level_warmer,
            level_warmer_error,
            last_level_warmer_event: None,
            displayed_level: None,
            render_level_hint: None,
            overview_cache: OverviewTileCache::default(),
        }
    }

    pub(super) fn viewer_open_options(&self) -> Result<ViewerOpenOptions, String> {
        self.tiles.viewer_open_options()
    }

    pub(super) fn backend_warning(&self) -> Option<&str> {
        self.tiles.backend_warning()
    }
    pub(super) fn clear(&mut self) {
        self.tiles.clear();
        if let Some(warmer) = &self.level_warmer {
            warmer.clear();
        }
        self.last_level_warmer_event = None;
        self.displayed_level = None;
        self.render_level_hint = None;
        self.overview_cache.clear();
    }

    pub(super) fn tile_failure(&self) -> Option<&TileFailureInfo> {
        self.tiles.tile_failure()
    }

    pub(super) fn cpu_fallback(&self) -> Option<(usize, &str)> {
        self.tiles.cpu_fallback()
    }

    pub(super) fn record_app_ui_cpu_time(&mut self, elapsed: Duration) {
        self.tiles.record_app_ui_cpu_time(elapsed);
    }

    pub(super) fn record_zoom_input(&mut self) {
        self.tiles.record_zoom_input();
    }

    pub(super) fn debug_stats_enabled(&self) -> bool {
        self.tiles.debug_stats_enabled()
    }

    pub(super) fn debug_stats_text(&self) -> Option<String> {
        use std::fmt::Write as _;

        let mut text = self.tiles.debug_stats_text()?;
        if let Some(warmer) = &self.level_warmer {
            let stats = warmer.stats();
            let _ = write!(
                text,
                "\nindex warm={}/{} active={:?} failed={} cancelled={}",
                stats.prepared_levels,
                stats.prepared_levels + stats.pending_levels,
                stats.active_level,
                stats.failed_total,
                stats.cancelled_total,
            );
        }
        if let Some(error) = &self.level_warmer_error {
            let _ = write!(text, "\n{error}");
        }
        if let Some(event) = &self.last_level_warmer_event {
            let _ = write!(text, "\n{event}");
        }
        Some(text)
    }

    pub(super) fn paint(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: Rect,
        study: &Arc<ViewerStudy>,
        generation: u64,
        camera: CameraFrame,
    ) {
        self.tiles.request_debug_stats_repaint(ctx);
        let summary = study.summary();
        let Some(base_size) = base_size(summary) else {
            paint_center_text(painter, rect, "slide has no levels");
            return;
        };
        let slide_rect = Rect::from_min_size(
            rect.center() + (egui::Vec2::ZERO - camera.rendered.center_base) * camera.rendered.zoom,
            base_size * camera.rendered.zoom,
        );
        painter.rect_filled(slide_rect, 0.0, Color32::BLACK);

        let overview = self.overview_cache.tiles_for(summary, generation);
        let Ok(plan) = TileFramePlan::build(
            summary,
            rect,
            generation,
            overview,
            camera,
            self.displayed_level,
            self.render_level_hint,
        ) else {
            paint_center_text(painter, rect, "slide has no renderable level");
            return;
        };

        self.render_level_hint = Some(plan.render_level);
        self.tiles.set_cache_protection(
            plan.pinned.clone(),
            plan.overview.iter().map(|tile| tile.key).collect(),
        );
        let foreground = self.tiles.coverage(&plan.render_visible, plan.render_level);
        let foreground_work_pending = foreground.pending + foreground.missing > 0;
        let prefetch_coverage = self.tiles.coverage(&plan.prefetch, plan.prefetch_level);
        let activity = frame_activity(
            camera.animating,
            plan.prefetch_level != plan.render_level,
            foreground_work_pending,
            prefetch_coverage.pending,
            prefetch_coverage.missing,
        );
        self.tiles.set_interactive(
            activity.interactive,
            summary.tile_decode_backend != TileDecodeBackend::Cpu,
        );

        let mut background_prefetch = Vec::new();
        if activity.allow_background {
            background_prefetch.extend_from_slice(&plan.prefetch);
            for layer in &plan.fallback_prefetch_layers {
                background_prefetch.extend_from_slice(&layer.tiles);
            }
        }
        let transition_target = if activity.transition_target {
            plan.prefetch.as_slice()
        } else {
            &[]
        };
        let mut cache_relevant = plan.pinned.clone();
        cache_relevant.extend(transition_target.iter().map(|tile| tile.key));
        cache_relevant.extend(background_prefetch.iter().map(|tile| tile.key));
        self.tiles.publish_frame_demand(
            Arc::clone(study),
            FrameTileDemand {
                study_generation: generation,
                current_visible: &plan.render_visible,
                transition_target,
                fallback_visible: &plan.fallback_visible,
                overview: &plan.overview,
                background_prefetch: &background_prefetch,
                cache_relevant: &cache_relevant,
            },
        );
        self.update_level_warmer(ctx, study, generation, &plan);
        self.tiles.poll_results(
            ctx,
            TilePollRequest {
                generation,
                relevant_tiles: &cache_relevant,
                visible_tiles: &plan.render_visible,
                transition_target_tiles: transition_target,
                fallback_tiles: &plan.fallback_visible,
                overview_tiles: &plan.overview,
                background_prefetch_tiles: &background_prefetch,
                render_level_index: plan.render_level,
                interactive: activity.transition_target,
            },
        );

        let (interaction_level, interaction_tiles) =
            interaction_measurement_target(&plan, activity);
        let interaction_coverage = self.tiles.coverage(interaction_tiles, interaction_level);
        self.tiles
            .observe_interaction_target(interaction_level, interaction_coverage);

        let target_uncovered = self
            .tiles
            .uncovered_tile_count(&plan.render_visible, plan.render_level);
        let held_uncovered = plan.held_level.map_or(0, |level| {
            self.tiles.uncovered_tile_count(&plan.held_visible, level)
        });
        self.displayed_level = Some(choose_display_level_index(
            plan.held_level,
            plan.render_level,
            target_uncovered,
            held_uncovered,
        ));

        for layer in &plan.fallback_visible_layers {
            let Some(level) = level_by_index(summary, layer.level) else {
                continue;
            };
            for tile in &layer.tiles {
                self.tiles.draw_ready_tile(
                    painter,
                    rect,
                    level,
                    tile,
                    camera.rendered.center_base,
                    camera.rendered.zoom,
                );
            }
        }
        if let Some(render_level) = level_by_index(summary, plan.render_level) {
            for tile in &plan.render_visible {
                self.tiles.draw_ready_tile(
                    painter,
                    rect,
                    render_level,
                    tile,
                    camera.rendered.center_base,
                    camera.rendered.zoom,
                );
            }
        }

        painter.rect_stroke(
            slide_rect,
            CornerRadius::ZERO,
            Stroke::new(1.0, theme::HAIRLINE),
            StrokeKind::Inside,
        );
        if self.tiles.loading_count() > 0 {
            ctx.request_repaint();
        }
    }

    fn update_level_warmer(
        &mut self,
        ctx: &egui::Context,
        study: &Arc<ViewerStudy>,
        generation: u64,
        plan: &TileFramePlan,
    ) {
        let Some(warmer) = &self.level_warmer else {
            return;
        };
        let diagnostics_enabled = self.tiles.debug_stats_enabled();
        warmer.publish_study(
            Arc::clone(study),
            generation,
            level_warming_order(study.summary(), plan.render_level, plan.prefetch_level),
            diagnostics_enabled,
        );
        while let Ok(event) = warmer.try_recv() {
            if event.study_generation() != generation {
                continue;
            }
            let (elapsed, status) = level_preparation_observation(&event);
            self.tiles.record_level_preparation(elapsed, status);
            self.tiles.record_dicom_index_diagnostics(
                DicomIndexDiagnosticSource::Preparation,
                level_warmer_index_diagnostics(&event),
            );
            if let Some(diagnostic) = level_warmer_diagnostic(&event, diagnostics_enabled) {
                self.last_level_warmer_event = Some(diagnostic);
            }
        }
        let stats = warmer.stats();
        if stats.pending_levels > 0 || stats.active_level.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameActivity {
    interactive: bool,
    transition_target: bool,
    allow_background: bool,
}

fn frame_activity(
    camera_animating: bool,
    adjacent_level: bool,
    foreground_pending: bool,
    prefetch_pending: usize,
    prefetch_missing: usize,
) -> FrameActivity {
    let adjacent_preload_pending =
        adjacent_level && prefetch_pending.saturating_add(prefetch_missing) > 0;
    FrameActivity {
        interactive: camera_animating,
        transition_target: camera_animating || adjacent_preload_pending,
        allow_background: !camera_animating && !foreground_pending && !adjacent_preload_pending,
    }
}

fn interaction_measurement_target(
    plan: &TileFramePlan,
    activity: FrameActivity,
) -> (LevelIndex, &[VisibleTile]) {
    if activity.transition_target || plan.prefetch_level != plan.render_level {
        (plan.prefetch_level, plan.prefetch.as_slice())
    } else {
        (plan.render_level, plan.render_visible.as_slice())
    }
}

fn level_preparation_observation(event: &LevelWarmerEvent) -> (Duration, LevelPreparationStatus) {
    match event {
        LevelWarmerEvent::Prepared { elapsed, .. } => (*elapsed, LevelPreparationStatus::Prepared),
        LevelWarmerEvent::Failed { elapsed, .. } => (*elapsed, LevelPreparationStatus::Failed),
        LevelWarmerEvent::Cancelled { elapsed, .. } => {
            (*elapsed, LevelPreparationStatus::Cancelled)
        }
    }
}

fn level_warmer_index_diagnostics(event: &LevelWarmerEvent) -> &[DicomIndexDiagnostic] {
    match event {
        LevelWarmerEvent::Prepared {
            index_diagnostics, ..
        }
        | LevelWarmerEvent::Failed {
            index_diagnostics, ..
        }
        | LevelWarmerEvent::Cancelled {
            index_diagnostics, ..
        } => index_diagnostics,
    }
}

fn level_warmer_diagnostic(event: &LevelWarmerEvent, enabled: bool) -> Option<String> {
    if !enabled {
        return None;
    }
    Some(match event {
        LevelWarmerEvent::Prepared { level, elapsed, .. } => format!(
            "index level {level} prepared in {:.1} ms",
            elapsed.as_secs_f64() * 1_000.0
        ),
        LevelWarmerEvent::Failed {
            level,
            error,
            elapsed,
            ..
        } => format!(
            "index level {level} failed after {:.1} ms: {error}",
            elapsed.as_secs_f64() * 1_000.0
        ),
        LevelWarmerEvent::Cancelled { level, elapsed, .. } => format!(
            "index level {level} cancelled after {:.1} ms",
            elapsed.as_secs_f64() * 1_000.0
        ),
    })
}

#[derive(Debug)]
struct TileLayer {
    level: LevelIndex,
    tiles: Vec<VisibleTile>,
}

#[derive(Debug)]
struct TileFramePlan {
    render_level: LevelIndex,
    prefetch_level: LevelIndex,
    held_level: Option<LevelIndex>,
    render_visible: Vec<VisibleTile>,
    prefetch: Vec<VisibleTile>,
    overview: Arc<[VisibleTile]>,
    held_visible: Vec<VisibleTile>,
    fallback_visible_layers: Vec<TileLayer>,
    fallback_prefetch_layers: Vec<TileLayer>,
    fallback_visible: Vec<VisibleTile>,
    pinned: HashSet<super::tile::TileKey>,
}

struct FramePlanningBudget {
    remaining: usize,
}

impl FramePlanningBudget {
    const fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    fn visible_tiles(
        &mut self,
        rect: Rect,
        level: &LevelInfo,
        generation: u64,
        center_base: egui::Vec2,
        zoom: f32,
        margin: i64,
    ) -> Vec<VisibleTile> {
        let tiles = visible_tiles_with_limit(
            rect,
            level,
            generation,
            center_base,
            zoom,
            margin,
            self.remaining,
        );
        self.remaining = self.remaining.saturating_sub(tiles.len());
        tiles
    }
}

impl TileFramePlan {
    fn build(
        summary: &StudySummary,
        rect: Rect,
        generation: u64,
        overview: Arc<[VisibleTile]>,
        camera: CameraFrame,
        displayed_level: Option<LevelIndex>,
        previous_render_level: Option<LevelIndex>,
    ) -> Result<Self, ()> {
        let preferred_render_level = choose_render_level(summary, camera.rendered.zoom).ok_or(())?;
        let render_level = choose_render_level_with_hysteresis(
            summary,
            camera.rendered.zoom,
            previous_render_level,
        )
        .unwrap_or(preferred_render_level);
        let target_level = if camera.animating {
            choose_render_level(summary, camera.target.zoom).unwrap_or(render_level)
        } else {
            preferred_render_level
        };
        let mut planning_budget = FramePlanningBudget::new(MAX_FRAME_PLANNING_REFERENCES);

        let render_visible = planning_budget.visible_tiles(
            rect,
            render_level,
            generation,
            camera.rendered.center_base,
            camera.rendered.zoom,
            0,
        );
        let prefetch = if camera.animating {
            planning_budget.visible_tiles(
                rect,
                target_level,
                generation,
                camera.target.center_base,
                camera.target.zoom,
                0,
            )
        } else if target_level.index != render_level.index {
            planning_budget.visible_tiles(
                rect,
                target_level,
                generation,
                camera.rendered.center_base,
                camera.rendered.zoom,
                0,
            )
        } else {
            planning_budget.visible_tiles(
                rect,
                render_level,
                generation,
                camera.rendered.center_base,
                camera.rendered.zoom,
                PREFETCH_MARGIN_TILES,
            )
        };
        let held_level = displayed_level
            .and_then(|index| level_by_index(summary, index))
            .map(|level| level.index);
        let held_visible = held_level
            .and_then(|index| level_by_index(summary, index))
            .map(|level| {
                planning_budget.visible_tiles(
                    rect,
                    level,
                    generation,
                    camera.rendered.center_base,
                    camera.rendered.zoom,
                    0,
                )
            })
            .unwrap_or_default();
        let fallback_levels = fallback_levels(
            summary,
            render_level,
            held_level.and_then(|index| level_by_index(summary, index)),
        );
        let fallback_visible_layers = fallback_levels
            .iter()
            .map(|level| TileLayer {
                level: level.index,
                tiles: planning_budget.visible_tiles(
                    rect,
                    level,
                    generation,
                    camera.rendered.center_base,
                    camera.rendered.zoom,
                    0,
                ),
            })
            .collect::<Vec<_>>();
        let fallback_prefetch_layers = if camera.animating {
            Vec::new()
        } else {
            fallback_levels
                .iter()
                .map(|level| TileLayer {
                    level: level.index,
                    tiles: planning_budget.visible_tiles(
                        rect,
                        level,
                        generation,
                        camera.rendered.center_base,
                        camera.rendered.zoom,
                        PREFETCH_MARGIN_TILES,
                    ),
                })
                .collect::<Vec<_>>()
        };
        let fallback_visible = flatten_layers(&fallback_visible_layers);
        let mut pinned = HashSet::new();
        pinned.extend(render_visible.iter().map(|tile| tile.key));
        pinned.extend(held_visible.iter().map(|tile| tile.key));
        pinned.extend(fallback_visible.iter().map(|tile| tile.key));
        pinned.extend(overview.iter().map(|tile| tile.key));

        Ok(Self {
            render_level: render_level.index,
            prefetch_level: target_level.index,
            held_level,
            render_visible,
            prefetch,
            overview,
            held_visible,
            fallback_visible_layers,
            fallback_prefetch_layers,
            fallback_visible,
            pinned,
        })
    }
}

fn overview_tiles(summary: &StudySummary, generation: u64) -> Vec<VisibleTile> {
    let Some(level) = summary
        .levels
        .iter()
        .filter(|level| level.downsample.is_finite() && level.downsample > 0.0)
        .filter(|level| level.tile_layout.grid_size().is_some())
        .max_by(|a, b| {
            a.downsample
                .total_cmp(&b.downsample)
                .then_with(|| a.index.cmp(&b.index))
        })
    else {
        return Vec::new();
    };
    let Some((cols, rows)) = level.tile_layout.grid_size() else {
        return Vec::new();
    };
    let (tile_width, tile_height) = level.tile_layout.display_tile_size();
    let mut remaining = OVERVIEW_PIN_BYTES;
    let mut tiles = Vec::new();
    for (distance2, row, col) in nearest_grid_coordinates(cols, rows, MAX_PLANNED_TILES) {
        let x = col.saturating_mul(u64::from(tile_width));
        let y = row.saturating_mul(u64::from(tile_height));
        let width = level.width.saturating_sub(x).min(u64::from(tile_width));
        let height = level.height.saturating_sub(y).min(u64::from(tile_height));
        let bytes = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .and_then(|bytes| usize::try_from(bytes).ok())
            .unwrap_or(usize::MAX);
        if bytes <= remaining {
            remaining -= bytes;
            tiles.push(VisibleTile {
                key: super::tile::TileKey {
                    generation,
                    level: level.index,
                    coord: dicom_viewer_core::TileCoord::new(col, row),
                },
                distance2,
            });
        }
    }
    tiles
}

fn level_warming_order(
    summary: &StudySummary,
    rendered_level: LevelIndex,
    transition_target: LevelIndex,
) -> Vec<LevelIndex> {
    let mut priorities = Vec::with_capacity(summary.levels.len());
    let mut seen = HashSet::with_capacity(summary.levels.len());
    for priority in [rendered_level, transition_target] {
        if summary.levels.iter().any(|level| level.index == priority) && seen.insert(priority) {
            priorities.push(priority);
        }
    }

    let overview = summary
        .levels
        .iter()
        .filter(|level| level.downsample.is_finite() && level.downsample > 0.0)
        .max_by(|a, b| {
            a.downsample
                .total_cmp(&b.downsample)
                .then_with(|| a.index.cmp(&b.index))
        })
        .map(|level| level.index);
    if let Some(overview) = overview.filter(|level| seen.insert(*level)) {
        priorities.push(overview);
    }

    let target_downsample = level_by_index(summary, transition_target)
        .map(|level| level.downsample)
        .filter(|downsample| downsample.is_finite())
        .unwrap_or(1.0);
    let mut remaining = summary
        .levels
        .iter()
        .filter(|level| !seen.contains(&level.index))
        .collect::<Vec<_>>();
    remaining.sort_by(|a, b| {
        let distance = |downsample: f64| {
            if downsample.is_finite() {
                (downsample - target_downsample).abs()
            } else {
                f64::INFINITY
            }
        };
        distance(a.downsample)
            .total_cmp(&distance(b.downsample))
            .then_with(|| b.downsample.total_cmp(&a.downsample))
            .then_with(|| a.index.cmp(&b.index))
    });
    priorities.extend(remaining.into_iter().map(|level| level.index));
    priorities
}

pub(super) fn fallback_levels<'a>(
    summary: &'a StudySummary,
    render_level: &LevelInfo,
    held_level: Option<&'a LevelInfo>,
) -> Vec<&'a LevelInfo> {
    let mut seen = HashSet::new();
    let mut levels = Vec::new();
    for level in &summary.levels {
        if level.index == render_level.index || level.tile_layout.grid_size().is_none() {
            continue;
        }
        let is_held = held_level.is_some_and(|held| held.index == level.index);
        let is_coarser = level.downsample > render_level.downsample;
        let (width, height) = level.tile_layout.display_tile_size();
        let is_bounded_fallback = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .is_some_and(|bytes| bytes <= MAX_FALLBACK_TILE_BYTES);
        if (is_held || (is_coarser && is_bounded_fallback)) && seen.insert(level.index) {
            levels.push(level);
        }
    }
    levels.sort_by(|a, b| {
        b.downsample
            .total_cmp(&a.downsample)
            .then_with(|| b.index.cmp(&a.index))
    });
    levels
}

fn flatten_layers(layers: &[TileLayer]) -> Vec<VisibleTile> {
    layers
        .iter()
        .flat_map(|layer| layer.tiles.iter().copied())
        .collect()
}

fn paint_center_text(painter: &egui::Painter, rect: Rect, message: &str) {
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        message,
        FontId::proportional(14.0),
        theme::TEXT_MUTED,
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use dicom_viewer_core::{LevelTileLayout, SourceKind, TileDecodeBackend};
    use eframe::egui::{pos2, vec2};

    use super::*;

    #[test]
    fn level_warmer_events_map_to_stats_without_disabled_diagnostic_formatting() {
        let event = LevelWarmerEvent::Failed {
            study_generation: 7,
            level: LevelIndex::from_u32(2),
            error: "synthetic index failure".into(),
            elapsed: Duration::from_millis(25),
            index_diagnostics: Vec::new(),
        };

        assert_eq!(
            level_preparation_observation(&event),
            (Duration::from_millis(25), LevelPreparationStatus::Failed)
        );
        assert!(level_warmer_diagnostic(&event, false).is_none());
        assert!(level_warmer_diagnostic(&event, true)
            .unwrap()
            .contains("synthetic index failure"));
        assert_eq!(event.study_generation(), 7);
    }

    #[test]
    fn stale_level_warmer_event_is_not_owned_by_the_replacement_study() {
        let stale = LevelWarmerEvent::Prepared {
            study_generation: 4,
            level: LevelIndex::from_u32(0),
            elapsed: Duration::from_millis(10),
            index_diagnostics: Vec::new(),
        };

        assert_ne!(stale.study_generation(), 5);
    }

    #[test]
    fn stationary_hysteresis_preloads_then_settles_and_allows_background() {
        let pending = frame_activity(false, true, false, 0, 1);
        assert!(!pending.interactive);
        assert!(pending.transition_target);
        assert!(!pending.allow_background);

        let settled = frame_activity(false, true, false, 0, 0);
        assert!(!settled.interactive);
        assert!(!settled.transition_target);
        assert!(settled.allow_background);
    }

    fn summary() -> StudySummary {
        StudySummary {
            source_path: PathBuf::from("slide.svs"),
            source_kind: SourceKind::File,
            format_label: "Aperio WSI".into(),
            tile_decode_backend: TileDecodeBackend::Cpu,
            file_count: 1,
            dicom_instance_count: 0,
            canvas_dimensions: (1024, 1024),
            levels: vec![
                LevelInfo {
                    index: LevelIndex::from_u32(0),
                    width: 1024,
                    height: 1024,
                    downsample: 1.0,
                    tile_layout: LevelTileLayout::Regular {
                        tile_width: 256,
                        tile_height: 256,
                        tiles_across: 4,
                        tiles_down: 4,
                    },
                },
                LevelInfo {
                    index: LevelIndex::from_u32(1),
                    width: 256,
                    height: 256,
                    downsample: 4.0,
                    tile_layout: LevelTileLayout::Regular {
                        tile_width: 256,
                        tile_height: 256,
                        tiles_across: 1,
                        tiles_down: 1,
                    },
                },
            ],
            instances: Vec::new(),
            warnings: Vec::new(),
            mpp: None,
            objective_power: None,
            color_management: dicom_viewer_core::ColorManagementSummary::unprofiled(),
        }
    }

    #[test]
    fn frame_plan_collects_visible_fallback_prefetch_and_pins() {
        let summary = summary();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
        let view = CameraView {
            center_base: vec2(512.0, 512.0),
            zoom: 1.0,
        };

        let plan = TileFramePlan::build(
            &summary,
            rect,
            9,
            Arc::from(overview_tiles(&summary, 9)),
            CameraFrame {
                rendered: view,
                target: view,
                animating: false,
            },
            None,
            None,
        )
        .unwrap();

        assert!(!plan.render_visible.is_empty());
        assert!(!plan.fallback_visible.is_empty());
        assert!(plan.prefetch.len() >= plan.render_visible.len());
        assert!(plan
            .render_visible
            .iter()
            .all(|tile| plan.pinned.contains(&tile.key)));
        assert_eq!(plan.overview.len(), 1);
        assert_eq!(plan.overview[0].key.level, LevelIndex::from_u32(1));
        assert!(plan.pinned.contains(&plan.overview[0].key));
    }

    #[test]
    fn frame_plan_applies_one_budget_across_all_fallback_levels() {
        let mut summary = summary();
        summary.canvas_dimensions = (20_000, 20_000);
        summary.levels = (0..32)
            .map(|index| LevelInfo {
                index: LevelIndex::from_u32(index),
                width: 20_000,
                height: 20_000,
                downsample: f64::from(index + 1),
                tile_layout: LevelTileLayout::Regular {
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across: 20_000,
                    tiles_down: 20_000,
                },
            })
            .collect();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(20_000.0, 20_000.0));
        let view = CameraView {
            center_base: vec2(10_000.0, 10_000.0),
            zoom: 1.0,
        };

        let plan = TileFramePlan::build(
            &summary,
            rect,
            1,
            Arc::from(Vec::<VisibleTile>::new()),
            CameraFrame {
                rendered: view,
                target: view,
                animating: false,
            },
            None,
            None,
        )
        .unwrap();
        let planned_references = plan.render_visible.len()
            + plan.prefetch.len()
            + plan.held_visible.len()
            + plan
                .fallback_visible_layers
                .iter()
                .map(|layer| layer.tiles.len())
                .sum::<usize>()
            + plan
                .fallback_prefetch_layers
                .iter()
                .map(|layer| layer.tiles.len())
                .sum::<usize>();

        assert!(planned_references <= MAX_PLANNED_TILES + 1);
    }

    #[test]
    fn frame_plan_schedules_regular_1024_tile_as_clean_target_fallback() {
        let mut summary = summary();
        summary.canvas_dimensions = (4096, 4096);
        summary.levels = vec![
            LevelInfo {
                index: LevelIndex::from_u32(0),
                width: 4096,
                height: 4096,
                downsample: 1.0,
                tile_layout: LevelTileLayout::Regular {
                    tile_width: 256,
                    tile_height: 256,
                    tiles_across: 16,
                    tiles_down: 16,
                },
            },
            LevelInfo {
                index: LevelIndex::from_u32(1),
                width: 1024,
                height: 1024,
                downsample: 4.0,
                tile_layout: LevelTileLayout::Regular {
                    tile_width: 1024,
                    tile_height: 1024,
                    tiles_across: 1,
                    tiles_down: 1,
                },
            },
        ];
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
        let view = CameraView {
            center_base: vec2(2048.0, 2048.0),
            zoom: 1.0,
        };

        let plan = TileFramePlan::build(
            &summary,
            rect,
            9,
            Arc::from(overview_tiles(&summary, 9)),
            CameraFrame {
                rendered: view,
                target: view,
                animating: false,
            },
            None,
            None,
        )
        .unwrap();

        let fallback = plan
            .fallback_visible_layers
            .iter()
            .find(|layer| layer.level == LevelIndex::from_u32(1))
            .expect("the regular 1024x1024 coarser level must remain drawable");
        assert_eq!(fallback.tiles.len(), 1);
        assert!(plan
            .fallback_visible
            .iter()
            .any(|tile| tile.key == fallback.tiles[0].key));
        assert!(plan.pinned.contains(&fallback.tiles[0].key));
    }

    #[test]
    fn animated_frame_loads_rendered_tiles_before_target_lookahead() {
        let summary = summary();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(256.0, 256.0));
        let rendered = CameraView {
            center_base: vec2(128.0, 128.0),
            zoom: 1.0,
        };
        let target = CameraView {
            center_base: vec2(896.0, 896.0),
            zoom: 1.0,
        };

        let plan = TileFramePlan::build(
            &summary,
            rect,
            9,
            Arc::from(overview_tiles(&summary, 9)),
            CameraFrame {
                rendered,
                target,
                animating: true,
            },
            None,
            None,
        )
        .unwrap();
        let target_visible = visible_tiles(
            rect,
            &summary.levels[0],
            9,
            target.center_base,
            target.zoom,
            0,
        );

        assert!(plan
            .render_visible
            .iter()
            .all(|tile| plan.pinned.contains(&tile.key)));
        assert!(target_visible
            .iter()
            .all(|tile| plan.prefetch.iter().any(|queued| queued.key == tile.key)));
    }

    #[test]
    fn animated_zoom_tracks_the_target_pyramid_level_before_settling() {
        let summary = summary();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(256.0, 256.0));
        let plan = TileFramePlan::build(
            &summary,
            rect,
            9,
            Arc::from(overview_tiles(&summary, 9)),
            CameraFrame {
                rendered: CameraView {
                    center_base: vec2(512.0, 512.0),
                    zoom: 0.25,
                },
                target: CameraView {
                    center_base: vec2(512.0, 512.0),
                    zoom: 1.0,
                },
                animating: true,
            },
            None,
            None,
        )
        .unwrap();

        assert_eq!(plan.render_level, LevelIndex::from_u32(1));
        assert_eq!(plan.prefetch_level, LevelIndex::from_u32(0));
        assert!(plan
            .prefetch
            .iter()
            .all(|tile| tile.key.level == plan.prefetch_level));
    }

    #[test]
    fn hysteresis_holds_current_level_while_preloading_adjacent_target() {
        let summary = summary();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(256.0, 256.0));
        let view = CameraView {
            center_base: vec2(512.0, 512.0),
            zoom: 0.26,
        };

        let plan = TileFramePlan::build(
            &summary,
            rect,
            9,
            Arc::from(overview_tiles(&summary, 9)),
            CameraFrame {
                rendered: view,
                target: view,
                animating: false,
            },
            Some(LevelIndex::from_u32(1)),
            Some(LevelIndex::from_u32(1)),
        )
        .unwrap();

        assert_eq!(plan.render_level, LevelIndex::from_u32(1));
        assert_eq!(plan.prefetch_level, LevelIndex::from_u32(0));
        assert!(plan
            .prefetch
            .iter()
            .all(|tile| tile.key.level == LevelIndex::from_u32(0)));
    }

    #[test]
    fn stationary_hysteresis_interaction_keeps_measuring_the_adjacent_target() {
        let summary = summary();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(256.0, 256.0));
        let view = CameraView {
            center_base: vec2(512.0, 512.0),
            zoom: 0.26,
        };
        let plan = TileFramePlan::build(
            &summary,
            rect,
            9,
            Arc::from(overview_tiles(&summary, 9)),
            CameraFrame {
                rendered: view,
                target: view,
                animating: false,
            },
            Some(LevelIndex::from_u32(1)),
            Some(LevelIndex::from_u32(1)),
        )
        .unwrap();

        for activity in [
            frame_activity(false, true, false, 0, 1),
            frame_activity(false, true, false, 0, 0),
        ] {
            let (level, tiles) = interaction_measurement_target(&plan, activity);
            assert_eq!(level, LevelIndex::from_u32(0));
            assert!(std::ptr::eq(tiles, plan.prefetch.as_slice()));
        }
    }

    #[test]
    fn overview_budget_selects_center_tiles_before_row_major_edges() {
        let mut summary = summary();
        summary.levels = vec![LevelInfo {
            index: LevelIndex::from_u32(4),
            width: 25_600,
            height: 25_600,
            downsample: 16.0,
            tile_layout: LevelTileLayout::Regular {
                tile_width: 256,
                tile_height: 256,
                tiles_across: 100,
                tiles_down: 100,
            },
        }];

        let tiles = overview_tiles(&summary, 7);

        assert_eq!(tiles.len(), 128);
        assert_eq!(
            tiles[0].key.coord,
            dicom_viewer_core::TileCoord::new(50, 50)
        );
        assert!(tiles
            .windows(2)
            .all(|pair| pair[0].distance2 <= pair[1].distance2));
    }

    #[test]
    fn overview_planning_is_bounded_for_a_huge_valid_grid() {
        let mut summary = summary();
        summary.levels = vec![LevelInfo {
            index: LevelIndex::from_u32(4),
            width: 256_000_000_000,
            height: 256_000_000_000,
            downsample: 1.0,
            tile_layout: LevelTileLayout::Regular {
                tile_width: 256,
                tile_height: 256,
                tiles_across: 1_000_000_000,
                tiles_down: 1_000_000_000,
            },
        }];

        let tiles = overview_tiles(&summary, 7);

        assert_eq!(tiles.len(), 128);
        assert_eq!(
            tiles[0].key.coord,
            dicom_viewer_core::TileCoord::new(500_000_000, 500_000_000)
        );
        assert!(tiles
            .windows(2)
            .all(|pair| pair[0].distance2 <= pair[1].distance2));
    }

    #[test]
    fn overview_cache_reuses_one_generation_and_rebuilds_after_generation_or_clear() {
        let summary = summary();
        let mut cache = OverviewTileCache::default();

        let first = cache.tiles_for(&summary, 7);
        let repeated = cache.tiles_for(&summary, 7);
        assert!(Arc::ptr_eq(&first, &repeated));

        let next_generation = cache.tiles_for(&summary, 8);
        assert!(!Arc::ptr_eq(&first, &next_generation));

        cache.clear();
        let after_clear = cache.tiles_for(&summary, 8);
        assert!(!Arc::ptr_eq(&next_generation, &after_clear));
    }

    #[test]
    fn level_warming_prioritizes_current_target_overview_then_nearest_levels() {
        let mut summary = summary();
        summary.levels = [
            (0, 1024_u64, 1.0),
            (1, 512_u64, 2.0),
            (2, 256_u64, 4.0),
            (3, 171_u64, 6.0),
            (4, 128_u64, 8.0),
        ]
        .into_iter()
        .map(|(index, size, downsample)| LevelInfo {
            index: LevelIndex::from_u32(index),
            width: size,
            height: size,
            downsample,
            tile_layout: LevelTileLayout::Regular {
                tile_width: 128,
                tile_height: 128,
                tiles_across: size.div_ceil(128),
                tiles_down: size.div_ceil(128),
            },
        })
        .collect();

        let priorities =
            level_warming_order(&summary, LevelIndex::from_u32(2), LevelIndex::from_u32(2));

        assert_eq!(
            priorities,
            vec![
                LevelIndex::from_u32(2),
                LevelIndex::from_u32(4),
                LevelIndex::from_u32(3),
                LevelIndex::from_u32(1),
                LevelIndex::from_u32(0),
            ]
        );
    }

    #[test]
    fn level_warming_keeps_distinct_render_and_transition_levels_first() {
        let summary = summary();

        assert_eq!(
            level_warming_order(&summary, LevelIndex::from_u32(1), LevelIndex::from_u32(0),),
            vec![LevelIndex::from_u32(1), LevelIndex::from_u32(0)]
        );
    }
}
