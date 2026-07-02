use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc, Condvar, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dicom_viewer_core::{LevelIndex, LevelInfo, TileCoord, ViewerStudy};
use eframe::egui::{
    self, pos2, Color32, ColorImage, CornerRadius, Rect, Stroke, StrokeKind, TextureHandle,
    TextureOptions,
};
use rayon::spawn_fifo;

use super::viewport::{tile_screen_rect, CanvasView};
use super::{
    request_next_frame, theme, LOADER_MESSAGE_BUDGET, MAX_LOADER_MESSAGES_PER_FRAME,
    MAX_PREFETCH_UPLOADS_PER_FRAME, MAX_VISIBLE_UPLOADS_PER_FRAME, PREFETCH_UPLOAD_BUDGET,
    VISIBLE_UPLOAD_BUDGET,
};

const TILE_DECODE_BATCH_SIZE: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct TileKey {
    pub(super) slide_id: u64,
    pub(super) level: LevelIndex,
    pub(super) coord: TileCoord,
}

#[cfg(test)]
impl TileKey {
    pub(super) fn for_test(slide_id: u64, level: u32, col: u64, row: u64) -> Self {
        Self {
            slide_id,
            level: LevelIndex::from_u32(level),
            coord: TileCoord::new(col, row),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct TileJobKey {
    pub(super) slide_id: u64,
    pub(super) tile: TileKey,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct VisibleTile {
    pub(super) key: TileKey,
    pub(super) distance2: u128,
}

pub(super) struct TilePollRequest<'a> {
    pub(super) slide_id: u64,
    pub(super) relevant_tiles: &'a HashSet<TileKey>,
    pub(super) visible_tiles: &'a [VisibleTile],
    pub(super) fallback_tiles: &'a [VisibleTile],
    pub(super) prefetch_tiles: &'a [VisibleTile],
    pub(super) render_level_index: LevelIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TilePriority {
    pub(super) lane: u8,
    pub(super) distance2: u128,
    pub(super) sequence: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum QueueLane {
    Visible,
    Fallback,
    Prefetch,
}

impl QueueLane {
    pub(super) const fn priority_lane(self) -> u8 {
        match self {
            Self::Fallback => 0,
            Self::Visible => 1,
            Self::Prefetch => 2,
        }
    }
}

struct TileJob {
    key: TileJobKey,
    priority: TilePriority,
    study: Arc<ViewerStudy>,
}

impl PartialEq for TileJob {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.key == other.key
    }
}

impl Eq for TileJob {}

impl PartialOrd for TileJob {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TileJob {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| self.key.cmp(&other.key))
    }
}

#[derive(Default)]
struct LoaderState {
    jobs: BinaryHeap<Reverse<TileJob>>,
    queued: HashMap<TileJobKey, TilePriority>,
    decoding: HashSet<TileJobKey>,
    in_flight: usize,
    shutdown: bool,
}

struct LoaderShared {
    state: Mutex<LoaderState>,
    available: Condvar,
}

struct TileLoader {
    shared: Arc<LoaderShared>,
    receiver: Receiver<TileLoaderMessage>,
    scheduler: Option<JoinHandle<()>>,
    sequence: u64,
}

enum TileLoaderMessage {
    Started(Vec<TileJobKey>),
    Finished(Vec<TileLoadResult>),
}

pub(super) struct TileLoadResult {
    pub(super) key: TileJobKey,
    pub(super) result: std::result::Result<DecodedTile, String>,
}

pub(super) struct DecodedTile {
    pub(super) image: ColorImage,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl DecodedTile {
    fn from_rgba_tile(tile: dicom_viewer_core::RgbaTile) -> Self {
        Self {
            image: ColorImage::from_rgba_unmultiplied(
                [tile.width as usize, tile.height as usize],
                &tile.rgba,
            ),
            width: tile.width,
            height: tile.height,
        }
    }
}

impl TileLoader {
    fn new() -> Self {
        Self::with_worker_count(default_worker_count())
    }

    fn with_worker_count(worker_count: usize) -> Self {
        let shared = Arc::new(LoaderShared {
            state: Mutex::new(LoaderState::default()),
            available: Condvar::new(),
        });
        let (sender, receiver) = mpsc::channel();
        let scheduler = (worker_count > 0)
            .then(|| spawn_tile_scheduler(Arc::clone(&shared), sender, worker_count));
        Self {
            shared,
            receiver,
            scheduler,
            sequence: 0,
        }
    }

    fn enqueue_or_reprioritize(
        &self,
        study: Arc<ViewerStudy>,
        key: TileJobKey,
        priority: TilePriority,
    ) -> bool {
        let mut state = self.shared.state.lock().expect("tile loader lock poisoned");
        if state.decoding.contains(&key) {
            return false;
        }
        if let Some(current_priority) = state.queued.get_mut(&key) {
            if priority < *current_priority {
                *current_priority = priority;
                state.jobs.push(Reverse(TileJob {
                    key,
                    priority,
                    study,
                }));
                drop(state);
                self.shared.available.notify_one();
            }
            return false;
        }
        state.queued.insert(key, priority);
        state.jobs.push(Reverse(TileJob {
            key,
            priority,
            study,
        }));
        drop(state);
        self.shared.available.notify_one();
        true
    }

    fn next_sequence(&mut self) -> u64 {
        let sequence = self.sequence;
        self.sequence = self.sequence.saturating_add(1);
        sequence
    }

    fn clear_queued(&self) {
        let mut state = self.shared.state.lock().expect("tile loader lock poisoned");
        state.jobs.clear();
        state.queued.clear();
    }

    fn retain_queued_tiles(&self, keep: &HashSet<TileKey>) {
        let mut state = self.shared.state.lock().expect("tile loader lock poisoned");
        state
            .jobs
            .retain(|Reverse(job)| keep.contains(&job.key.tile));
        state.queued.retain(|key, _| keep.contains(&key.tile));
    }

    fn try_recv(&self) -> std::result::Result<TileLoaderMessage, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for TileLoader {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().expect("tile loader lock poisoned");
            state.shutdown = true;
        }
        self.shared.available.notify_all();
        if let Some(scheduler) = self.scheduler.take() {
            let _ = scheduler.join();
        }
    }
}

fn spawn_tile_scheduler(
    shared: Arc<LoaderShared>,
    sender: Sender<TileLoaderMessage>,
    worker_limit: usize,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("dicom-viewer-tile-scheduler".into())
        .spawn(move || run_tile_scheduler(shared, sender, worker_limit))
        .expect("failed to spawn tile scheduler")
}

fn run_tile_scheduler(
    shared: Arc<LoaderShared>,
    sender: Sender<TileLoaderMessage>,
    worker_limit: usize,
) {
    loop {
        let jobs = {
            let mut state = shared.state.lock().expect("tile loader lock poisoned");
            loop {
                if state.shutdown {
                    return;
                }
                if state.in_flight < worker_limit {
                    if let Some(jobs) = pop_next_batch(&mut state, TILE_DECODE_BATCH_SIZE) {
                        break jobs;
                    }
                }
                state = shared
                    .available
                    .wait(state)
                    .expect("tile loader lock poisoned");
            }
        };

        let keys = jobs.iter().map(|job| job.key).collect::<Vec<_>>();
        let _ = sender.send(TileLoaderMessage::Started(keys.clone()));
        let task_shared = Arc::clone(&shared);
        let task_sender = sender.clone();
        spawn_fifo(move || {
            let results = decode_tile_jobs(jobs);
            let _ = task_sender.send(TileLoaderMessage::Finished(results));
            finish_dispatched_jobs(&task_shared, &keys);
        });
    }
}

fn pop_next_batch(state: &mut LoaderState, max_batch_size: usize) -> Option<Vec<TileJob>> {
    let first = pop_next_valid_job(state)?;
    let mut jobs = vec![first];
    while jobs.len() < max_batch_size {
        match state.jobs.peek() {
            Some(Reverse(job)) if !is_current_queued_job(state, job) => {
                state.jobs.pop();
            }
            Some(Reverse(job)) if can_batch_jobs(&jobs[0], job) => {
                if let Some(job) = pop_next_valid_job(state) {
                    jobs.push(job);
                }
            }
            _ => break,
        }
    }
    state.in_flight = state.in_flight.saturating_add(1);
    Some(jobs)
}

fn pop_next_valid_job(state: &mut LoaderState) -> Option<TileJob> {
    while let Some(Reverse(job)) = state.jobs.pop() {
        if is_current_queued_job(state, &job) {
            state.queued.remove(&job.key);
            state.decoding.insert(job.key);
            return Some(job);
        }
    }
    None
}

fn is_current_queued_job(state: &LoaderState, job: &TileJob) -> bool {
    state.queued.get(&job.key) == Some(&job.priority)
}

fn can_batch_jobs(first: &TileJob, next: &TileJob) -> bool {
    first.key.slide_id == next.key.slide_id
        && first.key.tile.level == next.key.tile.level
        && first.priority.lane == next.priority.lane
        && Arc::ptr_eq(&first.study, &next.study)
}

fn decode_tile_jobs(jobs: Vec<TileJob>) -> Vec<TileLoadResult> {
    if jobs.len() == 1 {
        return jobs.into_iter().map(decode_tile_job).collect();
    }

    let keys = jobs.iter().map(|job| job.key).collect::<Vec<_>>();
    let batch_result = catch_unwind(AssertUnwindSafe(|| {
        let requests = jobs
            .iter()
            .map(|job| (job.key.tile.level, job.key.tile.coord))
            .collect::<Vec<_>>();
        jobs[0].study.read_tiles_rgba(&requests)
    }));

    match batch_result {
        Ok(Ok(tiles)) if tiles.len() == jobs.len() => keys
            .into_iter()
            .zip(tiles)
            .map(|(key, tile)| TileLoadResult {
                key,
                result: Ok(DecodedTile::from_rgba_tile(tile)),
            })
            .collect(),
        Ok(Ok(tiles)) => keys
            .into_iter()
            .map(|key| TileLoadResult {
                key,
                result: Err(format!(
                    "tile batch returned {} tiles for {} jobs",
                    tiles.len(),
                    jobs.len()
                )),
            })
            .collect(),
        Ok(Err(_)) => jobs.into_iter().map(decode_tile_job).collect(),
        Err(_) => keys
            .into_iter()
            .map(|key| TileLoadResult {
                key,
                result: Err("tile decoder panicked".to_string()),
            })
            .collect(),
    }
}

fn decode_tile_job(job: TileJob) -> TileLoadResult {
    let key = job.key;
    let result = catch_unwind(AssertUnwindSafe(|| {
        job.study
            .read_tile_rgba(key.tile.level, key.tile.coord)
            .map(DecodedTile::from_rgba_tile)
            .map_err(|err| err.to_string())
    }))
    .unwrap_or_else(|_| Err("tile decoder panicked".to_string()));

    TileLoadResult { key, result }
}

fn finish_dispatched_jobs(shared: &LoaderShared, keys: &[TileJobKey]) {
    let mut state = shared.state.lock().expect("tile loader lock poisoned");
    for key in keys {
        state.decoding.remove(key);
    }
    state.in_flight = state.in_flight.saturating_sub(1);
    drop(state);
    shared.available.notify_one();
}

fn default_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().saturating_sub(1).clamp(2, 6))
        .unwrap_or(2)
}

pub(super) enum TileState {
    Queued,
    Decoding,
    Ready {
        texture: TextureHandle,
        width: u32,
        height: u32,
    },
    Decoded {
        tile: DecodedTile,
    },
    Failed,
}

pub(super) struct TileRenderer {
    loader: TileLoader,
    pub(super) cache: HashMap<TileKey, TileState>,
    pub(super) lru: VecDeque<TileKey>,
    pinned: HashSet<TileKey>,
    displayed_level: Option<LevelIndex>,
    max_ready_tiles: usize,
    pub(super) ready_count: usize,
    pub(super) loading_count: usize,
    failure_count: usize,
    last_failure: Option<TileFailureInfo>,
}

#[derive(Debug, Clone)]
pub(super) struct TileFailureInfo {
    pub(super) count: usize,
    pub(super) latest: String,
}

impl TileRenderer {
    pub(super) fn new(max_ready_tiles: usize) -> Self {
        Self {
            loader: TileLoader::new(),
            cache: HashMap::new(),
            lru: VecDeque::new(),
            pinned: HashSet::new(),
            displayed_level: None,
            max_ready_tiles,
            ready_count: 0,
            loading_count: 0,
            failure_count: 0,
            last_failure: None,
        }
    }

    #[cfg(test)]
    pub(super) fn new_for_tests(max_ready_tiles: usize) -> Self {
        Self {
            loader: TileLoader::with_worker_count(0),
            cache: HashMap::new(),
            lru: VecDeque::new(),
            pinned: HashSet::new(),
            displayed_level: None,
            max_ready_tiles,
            ready_count: 0,
            loading_count: 0,
            failure_count: 0,
            last_failure: None,
        }
    }

    pub(super) fn clear(&mut self) {
        self.loader.clear_queued();
        self.cache.clear();
        self.lru.clear();
        self.pinned.clear();
        self.displayed_level = None;
        self.ready_count = 0;
        self.loading_count = 0;
        self.failure_count = 0;
        self.last_failure = None;
    }

    pub(super) fn set_pinned(&mut self, pinned: HashSet<TileKey>) {
        self.pinned = pinned;
        self.lru.retain(|key| self.cache.contains_key(key));
        self.evict_ready_tiles();
    }

    pub(super) fn loading_count(&self) -> usize {
        self.loading_count
    }

    pub(super) fn tile_failure(&self) -> Option<&TileFailureInfo> {
        self.last_failure.as_ref()
    }

    pub(super) fn displayed_level(&self) -> Option<LevelIndex> {
        self.displayed_level
    }

    pub(super) fn set_displayed_level(&mut self, level: LevelIndex) {
        self.displayed_level = Some(level);
    }

    pub(super) fn retain_relevant_pending_tiles(&mut self, keep: &HashSet<TileKey>) {
        self.loader.retain_queued_tiles(keep);
        let mut removed = 0usize;
        self.cache.retain(|key, state| {
            let should_drop = matches!(state, TileState::Queued | TileState::Decoded { .. })
                && !keep.contains(key);
            if should_drop {
                removed += 1;
            }
            !should_drop
        });
        self.loading_count = self.loading_count.saturating_sub(removed);
    }

    pub(super) fn poll_results(&mut self, ctx: &egui::Context, request: TilePollRequest<'_>) {
        let mut processed = 0usize;
        let poll_started = Instant::now();
        while let Ok(message) = self.loader.try_recv() {
            processed += 1;
            match message {
                TileLoaderMessage::Started(keys) => {
                    for key in keys {
                        if is_stale_job(key, request.slide_id) {
                            continue;
                        }
                        if matches!(self.cache.get(&key.tile), Some(TileState::Queued)) {
                            self.cache.insert(key.tile, TileState::Decoding);
                        }
                    }
                }
                TileLoaderMessage::Finished(results) => {
                    for result in results {
                        self.stage_finished_tile(request.slide_id, request.relevant_tiles, result);
                    }
                }
            }
            if processed >= MAX_LOADER_MESSAGES_PER_FRAME
                || poll_started.elapsed() >= LOADER_MESSAGE_BUDGET
            {
                request_next_frame(ctx);
                break;
            }
        }

        self.upload_pending_tiles(
            ctx,
            request.visible_tiles,
            Some(request.render_level_index),
            MAX_VISIBLE_UPLOADS_PER_FRAME,
            VISIBLE_UPLOAD_BUDGET,
        );
        if self.pending_tile_count(request.visible_tiles, request.render_level_index) > 0 {
            self.upload_pending_tiles(
                ctx,
                request.fallback_tiles,
                None,
                MAX_VISIBLE_UPLOADS_PER_FRAME,
                VISIBLE_UPLOAD_BUDGET,
            );
        }
        if self.pending_tile_count(request.visible_tiles, request.render_level_index) == 0 {
            self.upload_pending_tiles(
                ctx,
                request.prefetch_tiles,
                Some(request.render_level_index),
                MAX_PREFETCH_UPLOADS_PER_FRAME,
                PREFETCH_UPLOAD_BUDGET,
            );
        }
    }

    pub(super) fn stage_finished_tile(
        &mut self,
        slide_id: u64,
        relevant_tiles: &HashSet<TileKey>,
        result: TileLoadResult,
    ) {
        if result.key.tile.slide_id != slide_id {
            return;
        }
        if !relevant_tiles.contains(&result.key.tile) {
            if self.cache.remove(&result.key.tile).is_some() {
                self.loading_count = self.loading_count.saturating_sub(1);
            }
            return;
        }
        if !self.cache.contains_key(&result.key.tile) {
            return;
        }
        match result.result {
            Ok(tile) => {
                if matches!(
                    self.cache.get(&result.key.tile),
                    Some(TileState::Ready { .. })
                ) {
                    self.loading_count = self.loading_count.saturating_sub(1);
                    return;
                }
                self.cache
                    .insert(result.key.tile, TileState::Decoded { tile });
            }
            Err(err) => {
                self.cache.insert(result.key.tile, TileState::Failed);
                self.loading_count = self.loading_count.saturating_sub(1);
                self.failure_count = self.failure_count.saturating_add(1);
                self.last_failure = Some(TileFailureInfo {
                    count: self.failure_count,
                    latest: format!(
                        "level {}, tile {},{}: {err}",
                        result.key.tile.level,
                        result.key.tile.coord.col(),
                        result.key.tile.coord.row()
                    ),
                });
            }
        }
    }

    fn upload_pending_tiles(
        &mut self,
        ctx: &egui::Context,
        tiles: &[VisibleTile],
        render_level_index: Option<LevelIndex>,
        max_uploads: usize,
        budget: Duration,
    ) -> usize {
        let started = Instant::now();
        let mut uploaded = 0usize;
        for tile in tiles {
            if uploaded >= max_uploads || started.elapsed() >= budget {
                request_next_frame(ctx);
                break;
            }
            if render_level_index.is_some_and(|index| tile.key.level != index) {
                continue;
            }
            if self.upload_pending_tile(ctx, tile.key) {
                uploaded += 1;
            }
        }
        uploaded
    }

    pub(super) fn upload_pending_tile(&mut self, ctx: &egui::Context, key: TileKey) -> bool {
        let Some(state) = self.cache.remove(&key) else {
            return false;
        };
        let TileState::Decoded { tile } = state else {
            self.cache.insert(key, state);
            return false;
        };

        let texture = ctx.load_texture(
            format!(
                "tile-{}-{}-{}-{}",
                key.slide_id,
                key.level,
                key.coord.col(),
                key.coord.row()
            ),
            tile.image,
            TextureOptions::NEAREST,
        );
        self.cache.insert(
            key,
            TileState::Ready {
                texture,
                width: tile.width,
                height: tile.height,
            },
        );
        self.ready_count += 1;
        self.loading_count = self.loading_count.saturating_sub(1);
        self.touch(key);
        self.evict_ready_tiles();
        true
    }

    pub(super) fn enqueue_tiles(
        &mut self,
        study: Arc<ViewerStudy>,
        tiles: &[VisibleTile],
        lane: QueueLane,
        render_level_index: LevelIndex,
    ) {
        for tile in tiles {
            if tile.key.level != render_level_index {
                continue;
            }
            let job_key = TileJobKey {
                slide_id: tile.key.slide_id,
                tile: tile.key,
            };
            let sequence = self.loader.next_sequence();
            let priority = TilePriority {
                lane: lane.priority_lane(),
                distance2: tile.distance2,
                sequence,
            };
            match self.cache.get(&tile.key) {
                Some(TileState::Queued) => {
                    self.loader
                        .enqueue_or_reprioritize(Arc::clone(&study), job_key, priority);
                }
                Some(
                    TileState::Decoding
                    | TileState::Decoded { .. }
                    | TileState::Ready { .. }
                    | TileState::Failed,
                ) => {}
                None => {
                    self.cache.insert(tile.key, TileState::Queued);
                    self.loading_count += 1;
                    self.loader
                        .enqueue_or_reprioritize(Arc::clone(&study), job_key, priority);
                }
            }
        }
    }

    pub(super) fn pending_tile_count(
        &self,
        tiles: &[VisibleTile],
        render_level_index: LevelIndex,
    ) -> usize {
        tiles
            .iter()
            .filter(|tile| tile.key.level == render_level_index)
            .filter(|tile| {
                matches!(
                    self.cache.get(&tile.key),
                    None | Some(
                        TileState::Queued | TileState::Decoding | TileState::Decoded { .. }
                    )
                )
            })
            .count()
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
        let Some(TileState::Ready {
            texture,
            width,
            height,
        }) = self.cache.get(&tile.key)
        else {
            if matches!(self.cache.get(&tile.key), Some(TileState::Failed)) {
                let (tile_w, tile_h) = level.tile_layout.display_tile_size();
                let failed_rect = tile_screen_rect(
                    CanvasView {
                        rect,
                        center_base,
                        zoom,
                    },
                    level,
                    tile.key.coord,
                    tile_w,
                    tile_h,
                );
                painter.rect_filled(
                    failed_rect,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(42, 28, 14, 150),
                );
                painter.rect_stroke(
                    failed_rect,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, theme::WARN),
                    StrokeKind::Inside,
                );
            }
            return false;
        };
        let texture_id = texture.id();
        let width = *width;
        let height = *height;
        self.touch(tile.key);
        painter.image(
            texture_id,
            tile_screen_rect(
                CanvasView {
                    rect,
                    center_base,
                    zoom,
                },
                level,
                tile.key.coord,
                width,
                height,
            ),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        true
    }

    pub(super) fn touch(&mut self, key: TileKey) {
        self.lru.retain(|existing| *existing != key);
        self.lru.push_back(key);
    }

    fn evict_ready_tiles(&mut self) {
        while self.ready_count > self.max_ready_tiles {
            let candidate = loop {
                let Some(key) = self.lru.pop_front() else {
                    return;
                };
                if self.pinned.contains(&key) {
                    continue;
                }
                if matches!(self.cache.get(&key), Some(TileState::Ready { .. })) {
                    break key;
                }
            };
            self.cache.remove(&candidate);
            self.ready_count = self.ready_count.saturating_sub(1);
        }
    }

    #[cfg(test)]
    pub(super) fn insert_ready_for_test(&mut self, key: TileKey, texture: TextureHandle) {
        self.cache.insert(
            key,
            TileState::Ready {
                texture,
                width: 1,
                height: 1,
            },
        );
        self.ready_count += 1;
        self.touch(key);
        self.evict_ready_tiles();
    }
}

pub(super) const fn is_stale_job(key: TileJobKey, active_slide_id: u64) -> bool {
    key.slide_id != active_slide_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finishing_dispatched_job_releases_parallel_slot() {
        let first = TileJobKey {
            slide_id: 1,
            tile: TileKey::for_test(1, 0, 2, 3),
        };
        let second = TileJobKey {
            slide_id: 1,
            tile: TileKey::for_test(1, 0, 3, 3),
        };
        let shared = LoaderShared {
            state: Mutex::new(LoaderState {
                decoding: HashSet::from([first, second]),
                in_flight: 1,
                ..LoaderState::default()
            }),
            available: Condvar::new(),
        };

        finish_dispatched_jobs(&shared, &[first, second]);

        let state = shared.state.lock().expect("tile loader lock poisoned");
        assert_eq!(state.in_flight, 0);
        assert!(state.decoding.is_empty());
    }

    #[test]
    fn rayon_scheduler_stops_on_shutdown() {
        let shared = Arc::new(LoaderShared {
            state: Mutex::new(LoaderState::default()),
            available: Condvar::new(),
        });
        let (sender, receiver) = mpsc::channel();
        let scheduler = spawn_tile_scheduler(Arc::clone(&shared), sender, 1);

        {
            let mut state = shared.state.lock().expect("tile loader lock poisoned");
            state.shutdown = true;
        }
        shared.available.notify_all();

        scheduler.join().expect("tile scheduler should not panic");
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
}
