use std::collections::HashSet;
use std::sync::Arc;

use dicom_viewer_core::{
    LevelIndex, LevelInfo, StudySummary, TileDecodeBackend, ViewerOpenOptions, ViewerStudy,
};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Rect, Stroke, StrokeKind};

use super::camera::CameraView;
use super::theme;
use super::tile::{QueueLane, TileFailureInfo, TilePollRequest, TileRenderer, VisibleTile};
use super::viewport::{
    base_size, choose_display_level_index, choose_render_level, level_by_index, visible_tiles,
};

pub(super) const PREFETCH_MARGIN_TILES: i64 = 1;
const MAX_FALLBACK_TILE_PIXELS: u64 = 1_000_000;
const TILE_RESIDENT_CACHE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(super) struct CanvasCamera {
    pub(super) rendered: CameraView,
    pub(super) target: CameraView,
    pub(super) camera_animating: bool,
}

pub(super) struct SlideCanvas {
    tiles: TileRenderer,
}

impl SlideCanvas {
    pub(super) fn new(render_state: eframe::egui_wgpu::RenderState) -> Self {
        Self {
            tiles: TileRenderer::new(render_state, TILE_RESIDENT_CACHE_BYTES),
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
    }

    pub(super) fn tile_failure(&self) -> Option<&TileFailureInfo> {
        self.tiles.tile_failure()
    }

    pub(super) fn cpu_fallback(&self) -> Option<(usize, &str)> {
        self.tiles.cpu_fallback()
    }

    pub(super) fn paint(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: Rect,
        study: &Arc<ViewerStudy>,
        generation: u64,
        camera: CanvasCamera,
    ) {
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

        let Ok(plan) = TileFramePlan::build(
            summary,
            rect,
            generation,
            camera,
            self.tiles.displayed_level(),
        ) else {
            paint_center_text(painter, rect, "slide has no renderable level");
            return;
        };

        self.tiles.set_interactive(
            camera.camera_animating,
            summary.tile_decode_backend != TileDecodeBackend::Cpu,
        );
        self.tiles.set_pinned(plan.pinned.clone());
        self.tiles
            .retain_relevant_pending_tiles(&plan.relevant_tiles);
        self.tiles.poll_results(
            ctx,
            TilePollRequest {
                study,
                generation,
                relevant_tiles: &plan.relevant_tiles,
                visible_tiles: &plan.render_visible,
                fallback_tiles: &plan.fallback_visible,
                prefetch_tiles: &plan.prefetch,
                render_level_index: plan.render_level,
            },
        );

        for layer in &plan.fallback_visible_layers {
            self.tiles.enqueue_tiles(
                Arc::clone(study),
                &layer.tiles,
                QueueLane::Fallback,
                layer.level,
            );
        }
        self.tiles.enqueue_tiles(
            Arc::clone(study),
            &plan.render_visible,
            QueueLane::Visible,
            plan.render_level,
        );

        let target_pending = self
            .tiles
            .pending_tile_count(&plan.render_visible, plan.render_level);
        let held_pending = plan.held_level.map_or(0, |level| {
            self.tiles.pending_tile_count(&plan.held_visible, level)
        });
        self.tiles.set_displayed_level(choose_display_level_index(
            plan.held_level,
            plan.render_level,
            target_pending,
            held_pending,
        ));

        if camera.camera_animating {
            self.tiles.enqueue_tiles(
                Arc::clone(study),
                &plan.prefetch,
                QueueLane::Prefetch,
                plan.prefetch_level,
            );
        } else if target_pending == 0 {
            self.tiles.enqueue_tiles(
                Arc::clone(study),
                &plan.prefetch,
                QueueLane::Prefetch,
                plan.prefetch_level,
            );
            for layer in &plan.fallback_prefetch_layers {
                self.tiles.enqueue_tiles(
                    Arc::clone(study),
                    &layer.tiles,
                    QueueLane::Prefetch,
                    layer.level,
                );
            }
        }

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
    held_visible: Vec<VisibleTile>,
    fallback_visible_layers: Vec<TileLayer>,
    fallback_prefetch_layers: Vec<TileLayer>,
    fallback_visible: Vec<VisibleTile>,
    pinned: HashSet<super::tile::TileKey>,
    relevant_tiles: HashSet<super::tile::TileKey>,
}

impl TileFramePlan {
    fn build(
        summary: &StudySummary,
        rect: Rect,
        generation: u64,
        camera: CanvasCamera,
        displayed_level: Option<LevelIndex>,
    ) -> Result<Self, ()> {
        let render_level = choose_render_level(summary, camera.rendered.zoom).ok_or(())?;
        let target_level = if camera.camera_animating {
            choose_render_level(summary, camera.target.zoom).unwrap_or(render_level)
        } else {
            render_level
        };

        let render_visible = visible_tiles(
            rect,
            render_level,
            generation,
            camera.rendered.center_base,
            camera.rendered.zoom,
            0,
        );
        let prefetch = if camera.camera_animating {
            visible_tiles(
                rect,
                target_level,
                generation,
                camera.target.center_base,
                camera.target.zoom,
                0,
            )
        } else {
            visible_tiles(
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
                visible_tiles(
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
                tiles: visible_tiles(
                    rect,
                    level,
                    generation,
                    camera.rendered.center_base,
                    camera.rendered.zoom,
                    0,
                ),
            })
            .collect::<Vec<_>>();
        let fallback_prefetch_layers = if camera.camera_animating {
            Vec::new()
        } else {
            fallback_levels
                .iter()
                .map(|level| TileLayer {
                    level: level.index,
                    tiles: visible_tiles(
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

        let warming_level_change = held_level.is_some_and(|level| level != render_level.index);
        let mut relevant_tiles = pinned.clone();
        if camera.camera_animating {
            relevant_tiles.extend(prefetch.iter().map(|tile| tile.key));
        } else if !warming_level_change {
            relevant_tiles.extend(prefetch.iter().map(|tile| tile.key));
            relevant_tiles.extend(
                fallback_prefetch_layers
                    .iter()
                    .flat_map(|layer| layer.tiles.iter().map(|tile| tile.key)),
            );
        }

        Ok(Self {
            render_level: render_level.index,
            prefetch_level: target_level.index,
            held_level,
            render_visible,
            prefetch,
            held_visible,
            fallback_visible_layers,
            fallback_prefetch_layers,
            fallback_visible,
            pinned,
            relevant_tiles,
        })
    }
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
        let is_cheap_fallback = u64::from(width) * u64::from(height) <= MAX_FALLBACK_TILE_PIXELS;
        if (is_held || (is_coarser && is_cheap_fallback)) && seen.insert(level.index) {
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

    use dicom_viewer_core::{LevelTileLayout, SourceKind, TileDecodeBackend};
    use eframe::egui::{pos2, vec2};

    use super::*;

    fn summary() -> StudySummary {
        StudySummary {
            source_path: PathBuf::from("slide.svs"),
            source_kind: SourceKind::File,
            format_label: "Aperio WSI".into(),
            tile_decode_backend: TileDecodeBackend::Cpu,
            file_count: 1,
            dicom_instance_count: 0,
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
            CanvasCamera {
                rendered: view,
                target: view,
                camera_animating: false,
            },
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
        assert!(plan.pinned.is_subset(&plan.relevant_tiles));
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
            CanvasCamera {
                rendered,
                target,
                camera_animating: true,
            },
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
        assert!(target_visible
            .iter()
            .all(|tile| plan.relevant_tiles.contains(&tile.key)));
    }
}
