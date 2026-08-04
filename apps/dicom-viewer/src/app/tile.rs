use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dicom_viewer_core::{LevelIndex, LevelInfo, RenderTile, TileCoord, ViewerStudy};
use eframe::egui::{self, Rect};

use super::{
    LOADER_MESSAGE_BUDGET, MAX_LOADER_MESSAGES_PER_FRAME, MAX_PREFETCH_UPLOADS_PER_FRAME,
    MAX_TRANSITION_UPLOADS_PER_FRAME, MAX_VISIBLE_UPLOADS_PER_FRAME, PREFETCH_UPLOAD_BUDGET,
    VISIBLE_UPLOAD_BUDGET,
};

mod loader;
mod stats;
mod store;
mod upload;

const MAX_FRAME_TILE_JOBS: usize = 8_192;

use loader::{QueuedTileRequest, TileLoader, TileLoaderMessage};
pub(super) use stats::{DicomIndexDiagnosticSource, LevelPreparationStatus};
use stats::{PipelineStats, PIPELINE_SCHEMA_VERSION};
use store::{TileCoverage, TileDemandStatus, TileStore};
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct DemandEpoch(u64);

impl DemandEpoch {
    const INITIAL: Self = Self(0);

    fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

pub(super) struct FrameTileDemand<'a> {
    pub(super) study_generation: u64,
    pub(super) current_visible: &'a [VisibleTile],
    pub(super) transition_target: &'a [VisibleTile],
    pub(super) fallback_visible: &'a [VisibleTile],
    pub(super) overview: &'a [VisibleTile],
    pub(super) background_prefetch: &'a [VisibleTile],
    pub(super) cache_relevant: &'a HashSet<TileKey>,
}

pub(super) struct TilePollRequest<'a> {
    pub(super) generation: u64,
    pub(super) relevant_tiles: &'a HashSet<TileKey>,
    pub(super) visible_tiles: &'a [VisibleTile],
    pub(super) transition_target_tiles: &'a [VisibleTile],
    pub(super) fallback_tiles: &'a [VisibleTile],
    pub(super) overview_tiles: &'a [VisibleTile],
    pub(super) background_prefetch_tiles: &'a [VisibleTile],
    pub(super) render_level_index: LevelIndex,
    pub(super) interactive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum QueueLane {
    Visible,
    TransitionTarget,
    Fallback,
    Overview,
    Prefetch,
}

#[cfg(test)]
pub(super) const fn prefetch_queue_lane(interactive: bool) -> QueueLane {
    if interactive {
        QueueLane::TransitionTarget
    } else {
        QueueLane::Prefetch
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TilePriority {
    pub(super) lane: QueueLane,
    pub(super) distance2: u128,
    pub(super) sequence: u64,
}

pub(super) struct TileLoadResult {
    pub(super) key: TileKey,
    pub(super) outcome: TileLoadOutcome,
    pub(super) used_cpu_fallback: bool,
}

pub(super) enum TileLoadOutcome {
    Decoded(DecodedTile),
    Cancelled,
    Failed(TileFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TileFailure {
    pub(super) message: String,
}

impl TileFailure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub(super) enum DecodedTile {
    Cpu(dicom_viewer_core::RgbaTile),
    #[cfg(target_os = "macos")]
    Metal(dicom_viewer_core::MetalRenderTile),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TileMemoryCost {
    decoded_bytes: usize,
    texture_bytes: usize,
    upload_peak_bytes: usize,
}

impl DecodedTile {
    fn from_render_tile(tile: RenderTile) -> std::result::Result<Self, String> {
        match tile {
            RenderTile::Cpu(tile) => Ok(Self::Cpu(tile)),
            #[cfg(target_os = "macos")]
            RenderTile::Metal(tile) => Ok(Self::Metal(tile)),
            #[allow(unreachable_patterns)]
            _ => Err("tile backend returned an unsupported renderer output type".into()),
        }
    }

    fn from_rgba_tile(tile: dicom_viewer_core::RgbaTile) -> Self {
        Self::Cpu(tile)
    }

    fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Cpu(tile) => (tile.width, tile.height),
            #[cfg(target_os = "macos")]
            Self::Metal(tile) => (tile.width(), tile.height()),
        }
    }

    fn memory_cost(&self) -> Result<TileMemoryCost, String> {
        let (width, height) = self.dimensions();
        let texture_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| {
                format!("decoded tile dimensions {width}x{height} overflow RGBA texture bytes")
            })?;
        let decoded_bytes = match self {
            Self::Cpu(tile) => {
                if tile.rgba.len() != texture_bytes {
                    return Err(format!(
                        "decoded CPU tile dimensions {width}x{height} require {texture_bytes} RGBA bytes, got {}",
                        tile.rgba.len()
                    ));
                }
                tile.rgba.len()
            }
            #[cfg(target_os = "macos")]
            Self::Metal(tile) => {
                let image = tile
                    .resident_image()
                    .map_err(|error| format!("invalid decoded Metal tile: {error}"))?;
                metal_wgpu_interop::resident_allocation_len(image)
                    .map_err(|error| format!("invalid decoded Metal allocation: {error}"))?
            }
        };
        let upload_peak_bytes = decoded_bytes
            .checked_add(texture_bytes)
            .ok_or_else(|| format!("decoded tile {width}x{height} upload peak overflows usize"))?;
        Ok(TileMemoryCost {
            decoded_bytes,
            texture_bytes,
            upload_peak_bytes,
        })
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
    stats: PipelineStats,
    demand_epoch: DemandEpoch,
    demand_lanes: HashMap<TileKey, QueueLane>,
    active_demand_keys: HashSet<TileKey>,
    accepted_result_keys: HashSet<TileKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UploadPlanEntry {
    key: TileKey,
    lane: QueueLane,
}

#[derive(Debug, Default)]
struct UploadPlan {
    entries: Vec<UploadPlanEntry>,
    cpu_budget: Duration,
}

impl TileRenderer {
    pub(super) fn new(
        render_state: eframe::egui_wgpu::RenderState,
        max_ready_bytes: usize,
    ) -> Self {
        let stats = PipelineStats::from_environment();
        let loader = TileLoader::new(stats.is_enabled(), max_ready_bytes);
        let mut store = TileStore::with_eviction_diagnostics(max_ready_bytes, stats.is_enabled());
        if let Some(error) = loader.startup_error.clone() {
            store.record_failure(error);
        }
        Self {
            loader,
            store,
            uploader: WgpuTileUploader::new(render_state),
            stats,
            demand_epoch: DemandEpoch::INITIAL,
            demand_lanes: HashMap::new(),
            active_demand_keys: HashSet::new(),
            accepted_result_keys: HashSet::new(),
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
        self.uploader.clear_study_resources();
        self.stats.clear_study_counters();
        self.demand_lanes.clear();
        self.active_demand_keys.clear();
        self.accepted_result_keys.clear();
        self.demand_epoch = self.demand_epoch.next();
        if let Some(error) = self.loader.startup_error.clone() {
            self.store.record_failure(error);
        }
    }

    pub(super) fn set_cache_protection(
        &mut self,
        pinned: HashSet<TileKey>,
        overview_reserved: HashSet<TileKey>,
    ) {
        self.store.set_cache_protection(pinned, overview_reserved);
    }

    pub(super) fn set_interactive(&mut self, interactive: bool, prefers_device: bool) {
        self.stats.set_interactive(interactive);
        self.loader.set_interactive(interactive, prefers_device);
    }

    pub(super) fn record_app_ui_cpu_time(&mut self, elapsed: Duration) {
        self.stats.record_app_ui_cpu_time(elapsed);
    }

    pub(super) fn record_zoom_input(&mut self) {
        self.stats.record_zoom_input();
    }

    pub(super) fn record_level_preparation(
        &mut self,
        elapsed: Duration,
        status: LevelPreparationStatus,
    ) {
        self.stats.record_level_preparation(elapsed, status);
    }

    pub(super) fn record_dicom_index_diagnostics(
        &mut self,
        source: DicomIndexDiagnosticSource,
        diagnostics: &[dicom_viewer_core::DicomIndexDiagnostic],
    ) {
        self.stats
            .record_dicom_index_diagnostics(source, diagnostics);
    }

    pub(super) fn observe_interaction_target(&mut self, level: LevelIndex, coverage: TileCoverage) {
        self.stats.observe_target(
            level,
            coverage.ready,
            coverage.pending,
            coverage.failed,
            coverage.missing,
        );
    }

    pub(super) fn debug_stats_enabled(&self) -> bool {
        self.stats.is_enabled()
    }

    pub(super) fn debug_stats_text(&self) -> Option<String> {
        self.stats.overlay_text()
    }

    pub(super) fn request_debug_stats_repaint(&self, ctx: &egui::Context) {
        self.stats
            .request_periodic_repaint(|after| ctx.request_repaint_after(after));
    }

    pub(super) fn loading_count(&self) -> usize {
        self.store.loading_count_for(&self.active_demand_keys)
    }

    pub(super) fn tile_failure(&self) -> Option<&TileFailureInfo> {
        self.store.tile_failure()
    }

    pub(super) fn cpu_fallback(&self) -> Option<(usize, &str)> {
        self.store.cpu_fallback()
    }

    pub(super) fn publish_frame_demand(
        &mut self,
        study: Arc<ViewerStudy>,
        demand: FrameTileDemand<'_>,
    ) {
        let capped = cap_frame_requests_to(canonical_frame_requests(&demand), MAX_FRAME_TILE_JOBS);
        if capped.visible_dropped > 0 && self.stats.is_enabled() {
            eprintln!(
                "{{\"schema_version\":{PIPELINE_SCHEMA_VERSION},\"kind\":\"demand_overflow\",\"limit\":{MAX_FRAME_TILE_JOBS},\"visible_requested\":{},\"visible_retained\":{},\"visible_dropped\":{}}}",
                capped.visible_requested,
                capped.visible_requested.saturating_sub(capped.visible_dropped),
                capped.visible_dropped,
            );
        }
        let requested = capped.requests;
        let planned_keep = capped.planned_keys;
        let result_keys =
            accepted_result_keys(&planned_keep, demand.overview, demand.study_generation);
        debug_assert!(planned_keep.is_subset(demand.cache_relevant));
        debug_assert!(result_keys.is_subset(demand.cache_relevant));
        let lanes = requested
            .iter()
            .map(|(key, (lane, _))| (*key, *lane))
            .collect::<HashMap<_, _>>();
        update_demand_epoch(&mut self.demand_epoch, &mut self.demand_lanes, lanes);

        self.store.retain_relevant_pending_tiles(&planned_keep);
        let mut ordered = requested.into_iter().collect::<Vec<_>>();
        ordered.sort_by_key(|(key, (lane, distance2))| (*lane, *distance2, *key));
        let mut requests = Vec::with_capacity(ordered.len());
        for (key, (lane, distance2)) in ordered {
            let upload_peak_bytes = study
                .summary()
                .levels
                .iter()
                .find(|level| level.index == key.level)
                .ok_or_else(|| format!("level {} is not renderable", key.level))
                .and_then(|level| planned_upload_peak_bytes(level, key.coord));
            if self
                .store
                .reject_upload_peak_preflight(key, upload_peak_bytes)
            {
                continue;
            }
            let read_mode = match self.store.queue_for_demand(key) {
                TileDemandStatus::Queue(read_mode) => read_mode,
                TileDemandStatus::Ready => {
                    self.stats.record_cache_hit();
                    continue;
                }
                TileDemandStatus::Deduplicated => {
                    self.stats.record_deduplicated(1);
                    continue;
                }
                TileDemandStatus::Failed => continue,
            };
            requests.push(QueuedTileRequest {
                study: Arc::clone(&study),
                key,
                priority: TilePriority {
                    lane,
                    distance2,
                    sequence: self.loader.next_sequence(),
                },
                read_mode,
            });
        }
        let enqueue = self
            .loader
            .publish_frame_demand(self.demand_epoch, requests, &planned_keep);
        self.stats.record_enqueued(enqueue.inserted);
        self.stats.record_deduplicated(enqueue.deduplicated);
        self.store.discard_queued_tiles(&enqueue.dropped);
        self.active_demand_keys = planned_keep;
        self.accepted_result_keys = result_keys;
    }

    pub(super) fn poll_results(&mut self, ctx: &egui::Context, request: TilePollRequest<'_>) {
        debug_assert!(self.accepted_result_keys.is_subset(request.relevant_tiles));
        let poll_started = Instant::now();
        let mut finished_results = 0;
        for processed in 0..MAX_LOADER_MESSAGES_PER_FRAME {
            let Ok(message) = self.loader.try_recv() else {
                break;
            };
            match message {
                TileLoaderMessage::Started { batch_id, keys } => {
                    if self.loader.batch_is_current(batch_id) {
                        self.store.mark_decoding(&keys, request.generation);
                    }
                }
                TileLoaderMessage::Finished {
                    batch_id,
                    results,
                    metrics,
                    index_diagnostics,
                } => {
                    finished_results += results.len();
                    let keys = results.iter().map(|result| result.key).collect::<Vec<_>>();
                    let record_for_active_study =
                        loader_batch_belongs_to_generation(request.generation, &keys);
                    let current_keys = self.loader.current_keys_for_batch(batch_id, &keys);
                    let mut obsolete_results = 0;
                    for result in results {
                        if stage_loader_result_if_current(
                            &mut self.store,
                            request.generation,
                            &self.accepted_result_keys,
                            current_keys.contains(&result.key),
                            result,
                        ) {
                            obsolete_results += 1;
                            if record_for_active_study {
                                self.stats.record_stale_work();
                            }
                        }
                    }
                    self.loader.acknowledge_finished(batch_id, &keys);
                    if record_for_active_study {
                        self.stats.record_dicom_index_diagnostics(
                            DicomIndexDiagnosticSource::Read,
                            &index_diagnostics,
                        );
                        self.stats
                            .record_batch_with_obsolete(metrics, obsolete_results);
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

        let upload_plan = build_upload_plan(&self.store, &request, &self.accepted_result_keys);
        let upload_keys = upload_plan
            .entries
            .iter()
            .map(|entry| entry.key)
            .collect::<Vec<_>>();
        let planned_uploads = upload_keys.len();
        let upload_started = (planned_uploads > 0 && self.stats.is_enabled()).then(Instant::now);
        let outcome = self.store.upload_pending_tiles(
            &mut self.uploader,
            &upload_keys,
            upload_plan.cpu_budget,
        );
        if let Some(upload_started) = upload_started {
            self.stats
                .record_upload(upload_started.elapsed(), planned_uploads, outcome.uploaded);
        }
        self.stats.record_failures(outcome.failures);
        for elapsed in self.store.take_eviction_samples() {
            self.stats.record_eviction(elapsed);
        }
        if poll_requires_followup(finished_results, outcome.uploaded, outcome.failures)
            || outcome.deferred > 0
            || !outcome.cpu_retries.is_empty()
        {
            // CPU fallbacks remain encoded in TileState and are published with
            // the next complete frame demand. This avoids waking workers with a
            // partial priority view in the middle of an upload pass.
            ctx.request_repaint();
        }
        if self.stats.is_enabled() {
            self.stats.update_gauges(
                self.active_demand_keys.len(),
                self.loader.stats(),
                self.store.resident_bytes(),
                self.store.pinned_bytes(),
                self.uploader.submission_count(),
            );
        }
    }

    pub(super) fn uncovered_tile_count(
        &self,
        tiles: &[VisibleTile],
        render_level_index: LevelIndex,
    ) -> usize {
        self.store.uncovered_tile_count(tiles, render_level_index)
    }

    pub(super) fn coverage(
        &self,
        tiles: &[VisibleTile],
        render_level_index: LevelIndex,
    ) -> TileCoverage {
        self.store.coverage(tiles, render_level_index)
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

fn planned_texture_bytes(level: &LevelInfo, coord: TileCoord) -> Result<usize, String> {
    let (tile_width, tile_height) = level.tile_layout.display_tile_size();
    let x = coord
        .col()
        .checked_mul(u64::from(tile_width))
        .ok_or_else(|| {
            format!(
                "tile column {} overflows level {} pixel coordinates",
                coord.col(),
                level.index
            )
        })?;
    let y = coord
        .row()
        .checked_mul(u64::from(tile_height))
        .ok_or_else(|| {
            format!(
                "tile row {} overflows level {} pixel coordinates",
                coord.row(),
                level.index
            )
        })?;
    if x >= level.width || y >= level.height {
        return Err(format!(
            "tile {},{} is outside level {} dimensions {}x{}",
            coord.col(),
            coord.row(),
            level.index,
            level.width,
            level.height
        ));
    }
    let width = level.width.saturating_sub(x).min(u64::from(tile_width));
    let height = level.height.saturating_sub(y).min(u64::from(tile_height));
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            format!(
                "tile {},{} on level {} overflows final RGBA texture bytes",
                coord.col(),
                coord.row(),
                level.index
            )
        })
}

fn planned_upload_peak_bytes(level: &LevelInfo, coord: TileCoord) -> Result<usize, String> {
    planned_texture_bytes(level, coord)?
        .checked_mul(2)
        .ok_or_else(|| {
            format!(
                "tile {},{} on level {} overflows decoded-source plus RGBA texture bytes",
                coord.col(),
                coord.row(),
                level.index
            )
        })
}

const fn poll_requires_followup(finished_results: usize, uploaded: usize, failures: usize) -> bool {
    finished_results > 0 || uploaded > 0 || failures > 0
}

fn build_upload_plan(
    store: &TileStore,
    request: &TilePollRequest<'_>,
    upload_eligible: &HashSet<TileKey>,
) -> UploadPlan {
    let visible_coverage = store.coverage(request.visible_tiles, request.render_level_index);
    let fallback_active =
        visible_coverage.pending + visible_coverage.failed + visible_coverage.missing > 0;
    let foreground_has_work = store.has_pending_or_missing(request.visible_tiles)
        || (request.interactive && store.has_pending_or_missing(request.transition_target_tiles))
        || (fallback_active && store.has_pending_or_missing(request.fallback_tiles));
    let background_idle = !request.interactive && !foreground_has_work;
    let decoded = request
        .visible_tiles
        .iter()
        .chain(request.transition_target_tiles)
        .chain(request.fallback_tiles)
        .chain(request.overview_tiles)
        .chain(request.background_prefetch_tiles)
        .filter_map(|tile| store.is_decoded(tile.key).then_some(tile.key))
        .collect::<HashSet<_>>();

    assemble_upload_plan(
        request,
        &decoded,
        upload_eligible,
        fallback_active,
        background_idle,
    )
}

fn assemble_upload_plan(
    request: &TilePollRequest<'_>,
    decoded: &HashSet<TileKey>,
    upload_eligible: &HashSet<TileKey>,
    fallback_active: bool,
    background_idle: bool,
) -> UploadPlan {
    let mut lane_by_key = HashMap::new();
    record_upload_lanes(&mut lane_by_key, request.visible_tiles, QueueLane::Visible);
    if request.interactive {
        record_upload_lanes(
            &mut lane_by_key,
            request.transition_target_tiles,
            QueueLane::TransitionTarget,
        );
    }
    if fallback_active {
        record_upload_lanes(
            &mut lane_by_key,
            request.fallback_tiles,
            QueueLane::Fallback,
        );
    }
    record_upload_lanes(
        &mut lane_by_key,
        request.overview_tiles,
        QueueLane::Overview,
    );
    record_upload_lanes(
        &mut lane_by_key,
        request.background_prefetch_tiles,
        QueueLane::Prefetch,
    );

    let visible = upload_candidates(
        request.visible_tiles,
        QueueLane::Visible,
        decoded,
        upload_eligible,
        &lane_by_key,
    );
    let transition = if request.interactive {
        upload_candidates(
            request.transition_target_tiles,
            QueueLane::TransitionTarget,
            decoded,
            upload_eligible,
            &lane_by_key,
        )
    } else {
        Vec::new()
    };
    let fallback = if fallback_active {
        upload_candidates(
            request.fallback_tiles,
            QueueLane::Fallback,
            decoded,
            upload_eligible,
            &lane_by_key,
        )
    } else {
        Vec::new()
    };

    let transition_slots = transition.len().min(MAX_TRANSITION_UPLOADS_PER_FRAME);
    let visible_slots = MAX_VISIBLE_UPLOADS_PER_FRAME.saturating_sub(transition_slots);
    let mut entries = Vec::with_capacity(MAX_VISIBLE_UPLOADS_PER_FRAME);
    entries.extend(visible.into_iter().take(visible_slots));
    entries.extend(transition.into_iter().take(transition_slots));
    let fallback_slots = MAX_VISIBLE_UPLOADS_PER_FRAME.saturating_sub(entries.len());
    entries.extend(fallback.into_iter().take(fallback_slots));

    if entries.is_empty() && background_idle {
        let overview = upload_candidates(
            request.overview_tiles,
            QueueLane::Overview,
            decoded,
            upload_eligible,
            &lane_by_key,
        );
        let prefetch = upload_candidates(
            request.background_prefetch_tiles,
            QueueLane::Prefetch,
            decoded,
            upload_eligible,
            &lane_by_key,
        );
        entries.extend(overview.into_iter().take(MAX_PREFETCH_UPLOADS_PER_FRAME));
        let prefetch_slots = MAX_PREFETCH_UPLOADS_PER_FRAME.saturating_sub(entries.len());
        entries.extend(prefetch.into_iter().take(prefetch_slots));
    }

    let cpu_budget = if entries
        .first()
        .is_some_and(|entry| entry.lane <= QueueLane::Fallback)
        || !background_idle
    {
        VISIBLE_UPLOAD_BUDGET
    } else {
        PREFETCH_UPLOAD_BUDGET
    };
    UploadPlan {
        entries,
        cpu_budget,
    }
}

fn record_upload_lanes(
    lanes: &mut HashMap<TileKey, QueueLane>,
    tiles: &[VisibleTile],
    lane: QueueLane,
) {
    for tile in tiles {
        lanes.entry(tile.key).or_insert(lane);
    }
}

fn upload_candidates(
    tiles: &[VisibleTile],
    lane: QueueLane,
    decoded: &HashSet<TileKey>,
    upload_eligible: &HashSet<TileKey>,
    lane_by_key: &HashMap<TileKey, QueueLane>,
) -> Vec<UploadPlanEntry> {
    let mut seen = HashSet::new();
    tiles
        .iter()
        .filter(|tile| decoded.contains(&tile.key))
        .filter(|tile| upload_eligible.contains(&tile.key))
        .filter(|tile| lane_by_key.get(&tile.key) == Some(&lane))
        .filter(|tile| seen.insert(tile.key))
        .map(|tile| UploadPlanEntry {
            key: tile.key,
            lane,
        })
        .collect()
}

fn canonical_frame_requests(demand: &FrameTileDemand<'_>) -> HashMap<TileKey, (QueueLane, u128)> {
    let mut requested = HashMap::new();
    for (lane, tiles) in [
        (QueueLane::Visible, demand.current_visible),
        (QueueLane::TransitionTarget, demand.transition_target),
        (QueueLane::Fallback, demand.fallback_visible),
        (QueueLane::Overview, demand.overview),
        (QueueLane::Prefetch, demand.background_prefetch),
    ] {
        for tile in tiles {
            if tile.key.generation != demand.study_generation {
                continue;
            }
            requested
                .entry(tile.key)
                .and_modify(|current| {
                    if (lane, tile.distance2) < *current {
                        *current = (lane, tile.distance2);
                    }
                })
                .or_insert((lane, tile.distance2));
        }
    }
    requested
}

struct CappedFrameRequests {
    requests: HashMap<TileKey, (QueueLane, u128)>,
    planned_keys: HashSet<TileKey>,
    visible_requested: usize,
    visible_dropped: usize,
}

fn cap_frame_requests_to(
    requests: HashMap<TileKey, (QueueLane, u128)>,
    max_jobs: usize,
) -> CappedFrameRequests {
    let visible_requested = requests
        .values()
        .filter(|(lane, _)| *lane == QueueLane::Visible)
        .count();
    if requests.len() <= max_jobs {
        let planned_keys = planned_request_keys(&requests);
        return CappedFrameRequests {
            requests,
            planned_keys,
            visible_requested,
            visible_dropped: 0,
        };
    }

    let mut ordered = requests.into_iter().collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|(key, (lane, distance2))| {
        (demand_retention_rank(*lane), *distance2, *key)
    });
    ordered.truncate(max_jobs);
    let requests = ordered.into_iter().collect::<HashMap<_, _>>();
    let planned_keys = planned_request_keys(&requests);
    let visible_retained = requests
        .values()
        .filter(|(lane, _)| *lane == QueueLane::Visible)
        .count();
    CappedFrameRequests {
        requests,
        planned_keys,
        visible_requested,
        visible_dropped: visible_requested.saturating_sub(visible_retained),
    }
}

const fn demand_retention_rank(lane: QueueLane) -> u8 {
    match lane {
        QueueLane::Visible => 0,
        QueueLane::Overview => 1,
        QueueLane::TransitionTarget => 2,
        QueueLane::Fallback => 3,
        QueueLane::Prefetch => 4,
    }
}

fn planned_request_keys(requests: &HashMap<TileKey, (QueueLane, u128)>) -> HashSet<TileKey> {
    requests.keys().copied().collect()
}

fn accepted_result_keys(
    planned_keys: &HashSet<TileKey>,
    overview: &[VisibleTile],
    study_generation: u64,
) -> HashSet<TileKey> {
    let mut accepted = planned_keys.clone();
    accepted.extend(
        overview
            .iter()
            .filter(|tile| tile.key.generation == study_generation)
            .map(|tile| tile.key),
    );
    accepted
}

fn stage_loader_result(
    store: &mut TileStore,
    active_generation: u64,
    accepted_result_keys: &HashSet<TileKey>,
    mut result: TileLoadResult,
) -> bool {
    let accepted = accepted_result_keys.contains(&result.key);
    if !accepted {
        // TileStore has a broader cache-pinning set than the authoritative
        // frame demand. Convert obsolete work to cancellation before staging
        // so broad cache relevance cannot resurrect a pruned decode result.
        result.outcome = TileLoadOutcome::Cancelled;
        result.used_cpu_fallback = false;
    }
    let discarded = store.stage_finished(active_generation, accepted_result_keys, result);
    !accepted || discarded
}

fn stage_loader_result_if_current(
    store: &mut TileStore,
    active_generation: u64,
    accepted_result_keys: &HashSet<TileKey>,
    batch_is_current: bool,
    result: TileLoadResult,
) -> bool {
    if !batch_is_current
        && (!accepted_result_keys.contains(&result.key)
            || !matches!(&result.outcome, TileLoadOutcome::Decoded(_)))
    {
        return true;
    }
    stage_loader_result(store, active_generation, accepted_result_keys, result)
}

fn update_demand_epoch(
    epoch: &mut DemandEpoch,
    current: &mut HashMap<TileKey, QueueLane>,
    next: HashMap<TileKey, QueueLane>,
) {
    if *current != next {
        *epoch = epoch.next();
        *current = next;
    }
}

pub(super) const fn is_stale_job(key: TileKey, active_generation: u64) -> bool {
    key.generation != active_generation
}

fn loader_batch_belongs_to_generation(active_generation: u64, keys: &[TileKey]) -> bool {
    !keys.is_empty() && keys.iter().all(|key| key.generation == active_generation)
}

#[cfg(test)]
mod tests;
