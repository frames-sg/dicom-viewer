use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dicom_viewer_core::{LevelIndex, LevelInfo, RenderTile, TileCoord, ViewerStudy};
use eframe::egui::{self, Rect};

use super::{
    LOADER_MESSAGE_BUDGET, MAX_LOADER_MESSAGES_PER_FRAME, MAX_PREFETCH_UPLOADS_PER_FRAME,
    MAX_VISIBLE_UPLOADS_PER_FRAME, PREFETCH_UPLOAD_BUDGET, VISIBLE_UPLOAD_BUDGET,
};

mod loader;
mod store;
mod upload;

use loader::{QueuedTileRequest, TileLoader, TileLoaderMessage, TileReadMode};
use store::{QueueStatus, TileStore};
use upload::WgpuTileUploader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct TileKey {
    pub(super) generation: u64,
    pub(super) level: LevelIndex,
    pub(super) coord: TileCoord,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct VisibleTile {
    pub(super) key: TileKey,
    pub(super) distance2: u128,
}

pub(super) struct TilePollRequest<'a> {
    pub(super) study: &'a Arc<ViewerStudy>,
    pub(super) generation: u64,
    pub(super) relevant_tiles: &'a HashSet<TileKey>,
    pub(super) visible_tiles: &'a [VisibleTile],
    pub(super) fallback_tiles: &'a [VisibleTile],
    pub(super) prefetch_tiles: &'a [VisibleTile],
    pub(super) render_level_index: LevelIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum QueueLane {
    Fallback,
    Visible,
    Prefetch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TilePriority {
    pub(super) lane: QueueLane,
    pub(super) distance2: u128,
    pub(super) sequence: u64,
}

pub(super) struct TileLoadResult {
    pub(super) key: TileKey,
    pub(super) result: std::result::Result<DecodedTile, String>,
    pub(super) used_cpu_fallback: bool,
}

pub(super) enum DecodedTile {
    Cpu(dicom_viewer_core::RgbaTile),
    #[cfg(target_os = "macos")]
    Metal(dicom_viewer_core::MetalRenderTile),
}

impl DecodedTile {
    fn from_render_tile(tile: RenderTile) -> Self {
        match tile {
            RenderTile::Cpu(tile) => Self::Cpu(tile),
            #[cfg(target_os = "macos")]
            RenderTile::Metal(tile) => Self::Metal(tile),
            #[allow(unreachable_patterns)]
            _ => unreachable!("unsupported render tiles are rejected by the core"),
        }
    }

    fn from_rgba_tile(tile: dicom_viewer_core::RgbaTile) -> Self {
        Self::Cpu(tile)
    }

    #[cfg(test)]
    fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Cpu(tile) => (tile.width, tile.height),
            #[cfg(target_os = "macos")]
            Self::Metal(tile) => (tile.width(), tile.height()),
        }
    }

    fn decoded_byte_len(&self) -> usize {
        match self {
            Self::Cpu(tile) => tile.rgba.len(),
            #[cfg(target_os = "macos")]
            Self::Metal(tile) => tile.byte_len(),
        }
    }

    const fn is_cpu(&self) -> bool {
        matches!(self, Self::Cpu(_))
    }
}

#[derive(Debug, Clone)]
pub(super) struct TileFailureInfo {
    pub(super) count: usize,
    pub(super) latest: String,
}

pub(super) struct TileRenderer {
    loader: TileLoader,
    store: TileStore,
    uploader: WgpuTileUploader,
}

struct PendingUploadRequest<'a> {
    study: &'a Arc<ViewerStudy>,
    tiles: &'a [VisibleTile],
    lane: QueueLane,
    render_level_index: Option<LevelIndex>,
    max_uploads: usize,
    budget: Duration,
}

impl TileRenderer {
    pub(super) fn new(
        render_state: eframe::egui_wgpu::RenderState,
        max_ready_bytes: usize,
    ) -> Self {
        let loader = TileLoader::new();
        let mut store = TileStore::new(max_ready_bytes);
        if let Some(error) = loader.startup_error.clone() {
            store.record_failure(error);
        }
        Self {
            loader,
            store,
            uploader: WgpuTileUploader::new(render_state),
        }
    }

    pub(super) fn viewer_open_options(
        &self,
    ) -> Result<dicom_viewer_core::ViewerOpenOptions, String> {
        self.uploader.viewer_open_options()
    }

    pub(super) fn backend_warning(&self) -> Option<&str> {
        self.uploader.backend_warning()
    }

    pub(super) fn clear(&mut self) {
        self.loader.clear_queued();
        self.store.clear();
        if let Some(error) = self.loader.startup_error.clone() {
            self.store.record_failure(error);
        }
    }

    pub(super) fn set_pinned(&mut self, pinned: HashSet<TileKey>) {
        self.store.set_pinned(pinned);
    }

    pub(super) fn set_interactive(&self, interactive: bool, prefers_device: bool) {
        self.loader.set_interactive(interactive, prefers_device);
    }

    pub(super) fn loading_count(&self) -> usize {
        self.store.loading_count()
    }

    pub(super) fn tile_failure(&self) -> Option<&TileFailureInfo> {
        self.store.tile_failure()
    }

    pub(super) fn cpu_fallback(&self) -> Option<(usize, &str)> {
        self.store.cpu_fallback()
    }

    pub(super) fn displayed_level(&self) -> Option<LevelIndex> {
        self.store.displayed_level()
    }

    pub(super) fn set_displayed_level(&mut self, level: LevelIndex) {
        self.store.set_displayed_level(level);
    }

    pub(super) fn retain_relevant_pending_tiles(&mut self, keep: &HashSet<TileKey>) {
        self.loader.retain_queued_tiles(keep);
        self.store.retain_relevant_pending_tiles(keep);
    }

    pub(super) fn poll_results(&mut self, ctx: &egui::Context, request: TilePollRequest<'_>) {
        let poll_started = Instant::now();
        for processed in 0..MAX_LOADER_MESSAGES_PER_FRAME {
            let Ok(message) = self.loader.try_recv() else {
                break;
            };
            match message {
                TileLoaderMessage::Started(keys) => {
                    self.store.mark_decoding(&keys, request.generation);
                }
                TileLoaderMessage::Finished(results) => {
                    for result in results {
                        self.store.stage_finished(
                            request.generation,
                            request.relevant_tiles,
                            result,
                        );
                    }
                }
            }
            if processed + 1 == MAX_LOADER_MESSAGES_PER_FRAME
                || poll_started.elapsed() >= LOADER_MESSAGE_BUDGET
            {
                ctx.request_repaint();
                break;
            }
        }

        self.upload_pending_tiles(
            ctx,
            PendingUploadRequest {
                study: request.study,
                tiles: request.visible_tiles,
                lane: QueueLane::Visible,
                render_level_index: Some(request.render_level_index),
                max_uploads: MAX_VISIBLE_UPLOADS_PER_FRAME,
                budget: VISIBLE_UPLOAD_BUDGET,
            },
        );
        if self.pending_tile_count(request.visible_tiles, request.render_level_index) > 0 {
            self.upload_pending_tiles(
                ctx,
                PendingUploadRequest {
                    study: request.study,
                    tiles: request.fallback_tiles,
                    lane: QueueLane::Fallback,
                    render_level_index: None,
                    max_uploads: MAX_VISIBLE_UPLOADS_PER_FRAME,
                    budget: VISIBLE_UPLOAD_BUDGET,
                },
            );
        }
        if self.pending_tile_count(request.visible_tiles, request.render_level_index) == 0 {
            self.upload_pending_tiles(
                ctx,
                PendingUploadRequest {
                    study: request.study,
                    tiles: request.prefetch_tiles,
                    lane: QueueLane::Prefetch,
                    render_level_index: Some(request.render_level_index),
                    max_uploads: MAX_PREFETCH_UPLOADS_PER_FRAME,
                    budget: PREFETCH_UPLOAD_BUDGET,
                },
            );
        }
    }

    fn upload_pending_tiles(
        &mut self,
        ctx: &egui::Context,
        request: PendingUploadRequest<'_>,
    ) -> usize {
        let started = Instant::now();
        let mut keys = Vec::new();
        for tile in request.tiles {
            if keys.len() >= request.max_uploads || started.elapsed() >= request.budget {
                ctx.request_repaint();
                break;
            }
            if request
                .render_level_index
                .is_some_and(|index| tile.key.level != index)
            {
                continue;
            }
            if self.store.is_decoded(tile.key) {
                keys.push(tile.key);
            }
        }
        let outcome = self.store.upload_pending_tiles(&mut self.uploader, &keys);
        if !outcome.cpu_retries.is_empty() {
            let mut requests = Vec::with_capacity(outcome.cpu_retries.len());
            for key in outcome.cpu_retries {
                let distance2 = request
                    .tiles
                    .iter()
                    .find(|tile| tile.key == key)
                    .map_or(0, |tile| tile.distance2);
                requests.push(QueuedTileRequest {
                    study: Arc::clone(request.study),
                    key,
                    priority: TilePriority {
                        lane: request.lane,
                        distance2,
                        sequence: self.loader.next_sequence(),
                    },
                    read_mode: TileReadMode::CpuFallback,
                });
            }
            self.loader.enqueue_or_reprioritize_batch(requests);
        }
        outcome.uploaded
    }

    pub(super) fn enqueue_tiles(
        &mut self,
        study: Arc<ViewerStudy>,
        tiles: &[VisibleTile],
        lane: QueueLane,
        render_level_index: LevelIndex,
    ) {
        let mut requests = Vec::new();
        for tile in tiles {
            if tile.key.level != render_level_index {
                continue;
            }
            let status = self.store.queue(tile.key);
            if matches!(status, QueueStatus::Ignore) {
                continue;
            }
            let priority = TilePriority {
                lane,
                distance2: tile.distance2,
                sequence: self.loader.next_sequence(),
            };
            requests.push(QueuedTileRequest {
                study: Arc::clone(&study),
                key: tile.key,
                priority,
                read_mode: TileReadMode::Preferred,
            });
        }
        self.loader.enqueue_or_reprioritize_batch(requests);
    }

    pub(super) fn pending_tile_count(
        &self,
        tiles: &[VisibleTile],
        render_level_index: LevelIndex,
    ) -> usize {
        self.store.pending_tile_count(tiles, render_level_index)
    }

    pub(super) fn draw_ready_tile(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        level: &LevelInfo,
        tile: &VisibleTile,
        center_base: eframe::egui::Vec2,
        zoom: f32,
    ) -> bool {
        self.store
            .draw_ready_tile(painter, rect, level, tile, center_base, zoom)
    }
}

pub(super) const fn is_stale_job(key: TileKey, active_generation: u64) -> bool {
    key.generation != active_generation
}

#[cfg(test)]
mod tests;
