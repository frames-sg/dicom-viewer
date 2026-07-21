use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    mpsc::{self, Receiver, SyncSender},
    Arc, Condvar, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use dicom_viewer_core::{
    DicomIndexDiagnostic, ReadCancellationToken, ReadControl, ReadDiagnostic, ViewerStudy,
};

use super::{
    DecodedTile, DemandEpoch, QueueLane, TileFailure, TileKey, TileLoadOutcome, TileLoadResult,
    TilePriority,
};

const TILE_DECODE_BATCH_SIZE: usize = 8;
const DEFAULT_INTERACTIVE_TILE_DECODE_BATCH_SIZE: usize = 2;
const MAX_QUEUED_TILES: usize = 8_192;

pub(super) struct TileLoader {
    shared: Arc<LoaderShared>,
    receiver: Option<Receiver<TileLoaderMessage>>,
    workers: Vec<JoinHandle<()>>,
    pub(super) startup_error: Option<String>,
    sequence: u64,
}

pub(super) enum TileLoaderMessage {
    Started {
        batch_id: u64,
        keys: Vec<TileKey>,
    },
    Finished {
        batch_id: u64,
        results: Vec<TileLoadResult>,
        metrics: TileBatchMetrics,
        index_diagnostics: Vec<DicomIndexDiagnostic>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TileQueueWait {
    pub(super) lane: QueueLane,
    pub(super) milliseconds: f64,
}

#[derive(Debug, Clone, Default)]
pub(super) struct TileBatchMetrics {
    pub(super) queue_waits: Vec<TileQueueWait>,
    pub(super) source_read_ms: Option<f64>,
    pub(super) requested: usize,
    pub(super) admitted: usize,
    pub(super) returned: usize,
    pub(super) cpu_results: usize,
    pub(super) metal_results: usize,
    pub(super) retries: usize,
    pub(super) failures: usize,
    pub(super) source_cancellations: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TileLoaderStats {
    pub(super) queued: usize,
    pub(super) visible: usize,
    pub(super) transition: usize,
    pub(super) fallback: usize,
    pub(super) overview: usize,
    pub(super) prefetch: usize,
    pub(super) decoding: usize,
    pub(super) cancellations: u64,
}

struct LoaderShared {
    state: Mutex<LoaderState>,
    available: Condvar,
}

struct LoaderState {
    jobs: BinaryHeap<Reverse<TileJob>>,
    queued: HashMap<TileKey, QueuedJob>,
    decoding: HashMap<TileKey, u64>,
    in_flight: HashMap<u64, InFlightBatch>,
    next_batch_id: u64,
    demand_epoch: DemandEpoch,
    last_foreground_lane: Option<QueueLane>,
    max_batch_size: usize,
    interactive_batch_size: usize,
    metrics_enabled: bool,
    cancellations: u64,
    shutdown: bool,
}

impl LoaderState {
    fn new(metrics_enabled: bool) -> Self {
        Self {
            jobs: BinaryHeap::new(),
            queued: HashMap::new(),
            decoding: HashMap::new(),
            in_flight: HashMap::new(),
            next_batch_id: 0,
            demand_epoch: DemandEpoch::INITIAL,
            last_foreground_lane: None,
            max_batch_size: TILE_DECODE_BATCH_SIZE,
            interactive_batch_size: configured_interactive_batch_size(
                std::env::var("DICOM_VIEWER_INTERACTIVE_BATCH_SIZE")
                    .ok()
                    .as_deref(),
            ),
            metrics_enabled,
            cancellations: 0,
            shutdown: false,
        }
    }
}

impl Default for LoaderState {
    fn default() -> Self {
        Self::new(false)
    }
}

struct InFlightBatch {
    demand_epoch: DemandEpoch,
    lane: QueueLane,
    keys: Vec<TileKey>,
    token: ReadCancellationToken,
}

struct TileBatch {
    id: u64,
    jobs: Vec<TileJob>,
    control: ReadControl,
    index_diagnostics: Option<Arc<Mutex<Vec<DicomIndexDiagnostic>>>>,
}

struct TileJob {
    key: TileKey,
    priority: TilePriority,
    study: Arc<ViewerStudy>,
    read_mode: TileReadMode,
    enqueued_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TileReadMode {
    Preferred,
    CpuFallback,
}

trait TileSource: Send + Sync {
    fn read_tiles(
        &self,
        requests: &[(dicom_viewer_core::LevelIndex, dicom_viewer_core::TileCoord)],
        read_mode: TileReadMode,
        control: &ReadControl,
    ) -> std::result::Result<Vec<DecodedTile>, TileSourceError>;

    fn device_was_preferred(&self, read_mode: TileReadMode) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TileSourceError {
    Cancelled,
    Failed(String),
}

impl TileSourceError {
    fn from_viewer(error: dicom_viewer_core::ViewerError) -> Self {
        if error.is_cancelled() {
            Self::Cancelled
        } else {
            Self::Failed(error.to_string())
        }
    }
}

impl TileSource for ViewerStudy {
    fn read_tiles(
        &self,
        requests: &[(dicom_viewer_core::LevelIndex, dicom_viewer_core::TileCoord)],
        read_mode: TileReadMode,
        control: &ReadControl,
    ) -> std::result::Result<Vec<DecodedTile>, TileSourceError> {
        match read_mode {
            TileReadMode::Preferred => self
                .read_tiles_for_render_controlled(requests, control)
                .map_err(TileSourceError::from_viewer)?
                .into_iter()
                .map(|tile| DecodedTile::from_render_tile(tile).map_err(TileSourceError::Failed))
                .collect(),
            TileReadMode::CpuFallback => self
                .read_tiles_rgba_controlled(requests, control)
                .map(|tiles| tiles.into_iter().map(DecodedTile::from_rgba_tile).collect())
                .map_err(TileSourceError::from_viewer),
        }
    }

    fn device_was_preferred(&self, read_mode: TileReadMode) -> bool {
        read_mode == TileReadMode::Preferred
            && self.summary().tile_decode_backend != dicom_viewer_core::TileDecodeBackend::Cpu
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueuedJob {
    priority: TilePriority,
    read_mode: TileReadMode,
    enqueued_at: Option<Instant>,
}

pub(super) struct QueuedTileRequest {
    pub(super) study: Arc<ViewerStudy>,
    pub(super) key: TileKey,
    pub(super) priority: TilePriority,
    pub(super) read_mode: TileReadMode,
}

pub(super) struct EnqueueOutcome {
    pub(super) inserted: usize,
    pub(super) deduplicated: usize,
    pub(super) dropped: Vec<TileKey>,
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

impl TileLoader {
    pub(super) fn new(metrics_enabled: bool) -> Self {
        Self::with_worker_count_and_metrics(default_worker_count(), metrics_enabled)
    }

    #[cfg(test)]
    fn with_worker_count(worker_count: usize) -> Self {
        Self::with_worker_count_and_metrics(worker_count, false)
    }

    fn with_worker_count_and_metrics(worker_count: usize, metrics_enabled: bool) -> Self {
        let shared = Arc::new(LoaderShared {
            state: Mutex::new(LoaderState::new(metrics_enabled)),
            available: Condvar::new(),
        });
        let (sender, receiver) = mpsc::sync_channel(worker_count.saturating_mul(2).max(1));
        let mut workers = Vec::with_capacity(worker_count);
        let mut startup_errors = Vec::new();
        for worker_index in 0..worker_count {
            match spawn_worker(worker_index, Arc::clone(&shared), sender.clone()) {
                Ok(worker) => workers.push(worker),
                Err(err) => startup_errors
                    .push(format!("failed to start tile worker {worker_index}: {err}")),
            }
        }
        drop(sender);

        Self {
            shared,
            receiver: Some(receiver),
            workers,
            startup_error: (!startup_errors.is_empty()).then(|| startup_errors.join("; ")),
            sequence: 0,
        }
    }

    pub(super) fn next_sequence(&mut self) -> u64 {
        let sequence = self.sequence;
        self.sequence = self.sequence.saturating_add(1);
        sequence
    }

    pub(super) fn set_interactive(&self, interactive: bool, prefers_device: bool) {
        let mut state = lock_state(&self.shared);
        state.max_batch_size = if interactive && !prefers_device {
            state.interactive_batch_size
        } else {
            TILE_DECODE_BATCH_SIZE
        };
    }

    #[cfg(test)]
    pub(super) fn enqueue_or_reprioritize_batch(
        &self,
        requests: Vec<QueuedTileRequest>,
    ) -> EnqueueOutcome {
        let mut state = lock_state(&self.shared);
        let outcome = enqueue_requests(&mut state, requests, false);
        drop(state);
        self.shared.available.notify_all();
        outcome
    }

    pub(super) fn publish_frame_demand(
        &self,
        demand_epoch: DemandEpoch,
        requests: Vec<QueuedTileRequest>,
        keep: &HashSet<TileKey>,
    ) -> EnqueueOutcome {
        let mut state = lock_state(&self.shared);
        state.demand_epoch = demand_epoch;
        state.jobs.retain(|Reverse(job)| keep.contains(&job.key));
        state.queued.retain(|key, _| keep.contains(key));
        let obsolete_batches = state
            .in_flight
            .iter()
            .filter(|(_, batch)| {
                batch.demand_epoch < demand_epoch
                    && batch.keys.iter().all(|key| !keep.contains(key))
            })
            .map(|(&batch_id, batch)| (batch_id, batch.keys.clone(), batch.token.clone()))
            .collect::<Vec<_>>();
        let mut cancelled = 0;
        for (batch_id, keys, token) in obsolete_batches {
            if !token.is_cancelled() {
                token.cancel();
                cancelled += 1;
            }
            for key in keys {
                if state.decoding.get(&key) == Some(&batch_id) {
                    state.decoding.remove(&key);
                }
            }
        }
        state.cancellations = state.cancellations.saturating_add(cancelled);
        let outcome = enqueue_requests(&mut state, requests, true);
        drop(state);
        // All lanes are now visible under one lock. Only now may workers select.
        self.shared.available.notify_all();
        outcome
    }

    pub(super) fn clear_queued(&self) {
        let mut state = lock_state(&self.shared);
        state.cancellations = state.cancellations.saturating_add(cancel_in_flight(&state));
        state.jobs.clear();
        state.queued.clear();
        state.decoding.clear();
        state.in_flight.clear();
        state.last_foreground_lane = None;
    }

    pub(super) fn try_recv(&self) -> std::result::Result<TileLoaderMessage, mpsc::TryRecvError> {
        self.receiver
            .as_ref()
            .expect("tile loader receiver must exist before drop")
            .try_recv()
    }

    pub(super) fn batch_is_current(&self, batch_id: u64) -> bool {
        lock_state(&self.shared)
            .decoding
            .values()
            .any(|current| *current == batch_id)
    }

    pub(super) fn acknowledge_finished(&self, batch_id: u64, keys: &[TileKey]) -> Vec<TileKey> {
        finish_batch(&self.shared, batch_id, keys)
    }

    pub(super) fn stats(&self) -> TileLoaderStats {
        let state = lock_state(&self.shared);
        let mut stats = TileLoaderStats {
            queued: state.queued.len(),
            decoding: state.decoding.len(),
            cancellations: state.cancellations,
            ..TileLoaderStats::default()
        };
        for job in state.queued.values() {
            match job.priority.lane {
                QueueLane::Visible => stats.visible += 1,
                QueueLane::TransitionTarget => stats.transition += 1,
                QueueLane::Fallback => stats.fallback += 1,
                QueueLane::Overview => stats.overview += 1,
                QueueLane::Prefetch => stats.prefetch += 1,
            }
        }
        stats
    }
}

fn enqueue_requests(
    state: &mut LoaderState,
    requests: Vec<QueuedTileRequest>,
    authoritative_priority: bool,
) -> EnqueueOutcome {
    let mut inserted = 0;
    let mut deduplicated = 0;
    let mut dropped = Vec::new();
    for request in requests {
        if state.decoding.contains_key(&request.key) {
            deduplicated += 1;
            continue;
        }
        if let Some(current) = state.queued.get_mut(&request.key) {
            deduplicated += 1;
            let read_mode = if current.read_mode == TileReadMode::CpuFallback
                || request.read_mode == TileReadMode::CpuFallback
            {
                TileReadMode::CpuFallback
            } else {
                TileReadMode::Preferred
            };
            let priority = if authoritative_priority {
                request.priority
            } else {
                current.priority.min(request.priority)
            };
            if read_mode != current.read_mode || priority != current.priority {
                let enqueued_at = current.enqueued_at;
                *current = QueuedJob {
                    priority,
                    read_mode,
                    enqueued_at,
                };
                state.jobs.push(Reverse(TileJob {
                    key: request.key,
                    priority,
                    study: request.study,
                    read_mode,
                    enqueued_at,
                }));
            }
            continue;
        }
        if state.queued.len() >= MAX_QUEUED_TILES {
            let replace = state
                .queued
                .iter()
                .max_by_key(|(key, job)| (job.priority, **key))
                .filter(|(_, job)| job.priority > request.priority)
                .map(|(key, _)| *key);
            if let Some(replace) = replace {
                state.queued.remove(&replace);
                dropped.push(replace);
            } else {
                dropped.push(request.key);
                continue;
            }
        }
        let enqueued_at = state.metrics_enabled.then(Instant::now);
        state.queued.insert(
            request.key,
            QueuedJob {
                priority: request.priority,
                read_mode: request.read_mode,
                enqueued_at,
            },
        );
        state.jobs.push(Reverse(TileJob {
            key: request.key,
            priority: request.priority,
            study: request.study,
            read_mode: request.read_mode,
            enqueued_at,
        }));
        inserted += 1;
    }
    compact_stale_jobs(state);
    EnqueueOutcome {
        inserted,
        deduplicated,
        dropped,
    }
}

impl Drop for TileLoader {
    fn drop(&mut self) {
        {
            let mut state = lock_state(&self.shared);
            state.shutdown = true;
            state.cancellations = state.cancellations.saturating_add(cancel_in_flight(&state));
            state.jobs.clear();
            state.queued.clear();
        }
        drop(self.receiver.take());
        self.shared.available.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn spawn_worker(
    worker_index: usize,
    shared: Arc<LoaderShared>,
    sender: SyncSender<TileLoaderMessage>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("dicom-viewer-tile-{worker_index}"))
        .spawn(move || run_worker(&shared, &sender))
}

fn run_worker(shared: &LoaderShared, sender: &SyncSender<TileLoaderMessage>) {
    loop {
        let Some(batch) = wait_for_batch(shared) else {
            return;
        };
        let keys = batch.jobs.iter().map(|job| job.key).collect::<Vec<_>>();
        if sender
            .send(TileLoaderMessage::Started {
                batch_id: batch.id,
                keys: keys.clone(),
            })
            .is_err()
        {
            finish_batch(shared, batch.id, &keys);
            return;
        }

        let queue_waits = batch
            .jobs
            .first()
            .and_then(|job| job.enqueued_at)
            .map_or_else(Vec::new, |_| {
                queue_wait_samples(&batch.jobs, Instant::now())
            });
        let source_read_started = batch
            .jobs
            .first()
            .and_then(|job| job.enqueued_at.map(|_| Instant::now()));
        let requested = batch.jobs.len();
        let decoded = decode_tile_jobs(batch.jobs, &batch.control);
        let source_read_ms =
            source_read_started.map(|started| started.elapsed().as_secs_f64() * 1000.0);
        let cpu_results = decoded
            .results
            .iter()
            .filter(
                |result| matches!(&result.outcome, TileLoadOutcome::Decoded(tile) if tile.is_cpu()),
            )
            .count();
        let metal_results = decoded
            .results
            .iter()
            .filter(|result| matches!(&result.outcome, TileLoadOutcome::Decoded(tile) if !tile.is_cpu()))
            .count();
        let failures = decoded
            .results
            .iter()
            .filter(|result| matches!(&result.outcome, TileLoadOutcome::Failed(_)))
            .count();
        let metrics = TileBatchMetrics {
            queue_waits,
            source_read_ms,
            requested,
            admitted: decoded.admitted,
            returned: decoded.returned,
            cpu_results,
            metal_results,
            retries: decoded.retries,
            failures,
            source_cancellations: decoded.source_cancellations,
        };
        let index_diagnostics = take_index_diagnostics(&batch.index_diagnostics);
        if sender
            .send(TileLoaderMessage::Finished {
                batch_id: batch.id,
                results: decoded.results,
                metrics,
                index_diagnostics,
            })
            .is_err()
        {
            finish_batch(shared, batch.id, &keys);
            return;
        }
    }
}

fn queue_wait_samples(jobs: &[TileJob], observed_at: Instant) -> Vec<TileQueueWait> {
    jobs.iter()
        .filter_map(|job| {
            job.enqueued_at.map(|enqueued_at| TileQueueWait {
                lane: job.priority.lane,
                milliseconds: observed_at
                    .saturating_duration_since(enqueued_at)
                    .as_secs_f64()
                    * 1_000.0,
            })
        })
        .collect()
}

fn compact_stale_jobs(state: &mut LoaderState) {
    if state.jobs.len() <= MAX_QUEUED_TILES.saturating_mul(2) {
        return;
    }
    let queued = state.queued.clone();
    state.jobs.retain(|Reverse(job)| {
        queued.get(&job.key).is_some_and(|queued| {
            queued.priority == job.priority && queued.read_mode == job.read_mode
        })
    });
}

fn wait_for_batch(shared: &LoaderShared) -> Option<TileBatch> {
    let mut state = lock_state(shared);
    loop {
        if state.shutdown {
            return None;
        }
        if let Some(jobs) = pop_next_batch(&mut state) {
            let id = state.next_batch_id;
            state.next_batch_id = state.next_batch_id.saturating_add(1);
            let demand_epoch = state.demand_epoch;
            let lane = jobs[0].priority.lane;
            let keys = jobs.iter().map(|job| job.key).collect::<Vec<_>>();
            state
                .decoding
                .extend(keys.iter().copied().map(|key| (key, id)));
            let token = ReadCancellationToken::default();
            let index_diagnostics = state
                .metrics_enabled
                .then(|| Arc::new(Mutex::new(Vec::<DicomIndexDiagnostic>::new())));
            let mut control = ReadControl::new(token.clone());
            if let Some(diagnostics) = &index_diagnostics {
                let captured = Arc::clone(diagnostics);
                control = control.with_diagnostic_sink(Arc::new(move |event| {
                    if let ReadDiagnostic::DicomIndex(diagnostic) = event {
                        captured
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(diagnostic);
                    }
                }));
            }
            state.in_flight.insert(
                id,
                InFlightBatch {
                    demand_epoch,
                    lane,
                    keys,
                    token,
                },
            );
            if matches!(lane, QueueLane::Visible | QueueLane::TransitionTarget) {
                state.last_foreground_lane = Some(lane);
            }
            return Some(TileBatch {
                id,
                jobs,
                control,
                index_diagnostics,
            });
        }
        state = shared
            .available
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

fn take_index_diagnostics(
    diagnostics: &Option<Arc<Mutex<Vec<DicomIndexDiagnostic>>>>,
) -> Vec<DicomIndexDiagnostic> {
    diagnostics.as_ref().map_or_else(Vec::new, |events| {
        std::mem::take(
            &mut *events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    })
}

fn pop_next_batch(state: &mut LoaderState) -> Option<Vec<TileJob>> {
    let lane = choose_next_lane(state)?;
    let first = pop_next_valid_job_for_lane(state, lane)?;
    let mut jobs = vec![first];
    while jobs.len() < state.max_batch_size {
        let Some(job) = pop_next_compatible_job(state, &jobs[0]) else {
            break;
        };
        jobs.push(job);
    }
    Some(jobs)
}

fn choose_next_lane(state: &LoaderState) -> Option<QueueLane> {
    let has_visible = state
        .queued
        .values()
        .any(|job| job.priority.lane == QueueLane::Visible);
    let has_transition = state
        .queued
        .values()
        .any(|job| job.priority.lane == QueueLane::TransitionTarget);
    let transition_inflight = state
        .in_flight
        .values()
        .any(|batch| batch.lane == QueueLane::TransitionTarget && !batch.token.is_cancelled());
    if has_transition && transition_inflight {
        return has_visible.then_some(QueueLane::Visible);
    }
    if has_visible && has_transition {
        if !transition_inflight && state.last_foreground_lane == Some(QueueLane::Visible) {
            return Some(QueueLane::TransitionTarget);
        }
        return Some(QueueLane::Visible);
    }
    state.queued.values().map(|job| job.priority.lane).min()
}

fn pop_next_valid_job_for_lane(state: &mut LoaderState, lane: QueueLane) -> Option<TileJob> {
    let mut deferred = Vec::new();
    while let Some(Reverse(job)) = state.jobs.pop() {
        if !is_current_job(state, &job) {
            continue;
        }
        if job.priority.lane == lane {
            state.queued.remove(&job.key);
            state.jobs.extend(deferred.into_iter().map(Reverse));
            return Some(job);
        }
        deferred.push(job);
    }
    state.jobs.extend(deferred.into_iter().map(Reverse));
    None
}

fn pop_next_compatible_job(state: &mut LoaderState, first: &TileJob) -> Option<TileJob> {
    let mut deferred = Vec::new();
    while let Some(Reverse(job)) = state.jobs.pop() {
        if !is_current_job(state, &job) {
            continue;
        }
        if can_batch(first, &job) {
            state.queued.remove(&job.key);
            state.jobs.extend(deferred.into_iter().map(Reverse));
            return Some(job);
        }
        deferred.push(job);
    }
    state.jobs.extend(deferred.into_iter().map(Reverse));
    None
}

fn is_current_job(state: &LoaderState, job: &TileJob) -> bool {
    state
        .queued
        .get(&job.key)
        .is_some_and(|queued| queued.priority == job.priority && queued.read_mode == job.read_mode)
}

fn can_batch(first: &TileJob, next: &TileJob) -> bool {
    first.key.generation == next.key.generation
        && first.key.level == next.key.level
        && first.priority.lane == next.priority.lane
        && first.read_mode == next.read_mode
        && Arc::ptr_eq(&first.study, &next.study)
}

fn decode_tile_jobs(jobs: Vec<TileJob>, control: &ReadControl) -> TileDecodeBatch {
    let keys = jobs.iter().map(|job| job.key).collect::<Vec<_>>();
    let requests = jobs
        .iter()
        .map(|job| (job.key.level, job.key.coord))
        .collect::<Vec<_>>();
    let read_mode = jobs[0].read_mode;
    decode_source_requests(jobs[0].study.as_ref(), keys, &requests, read_mode, control)
}

struct TileDecodeBatch {
    results: Vec<TileLoadResult>,
    admitted: usize,
    returned: usize,
    retries: usize,
    source_cancellations: usize,
}

fn decode_source_requests(
    source: &dyn TileSource,
    keys: Vec<TileKey>,
    requests: &[(dicom_viewer_core::LevelIndex, dicom_viewer_core::TileCoord)],
    read_mode: TileReadMode,
    control: &ReadControl,
) -> TileDecodeBatch {
    if control.cancellation().is_cancelled() {
        return TileDecodeBatch {
            results: cancelled_tiles(keys),
            admitted: 0,
            returned: 0,
            retries: 0,
            source_cancellations: 0,
        };
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        source.read_tiles(requests, read_mode, control)
    }));
    let returned = match &outcome {
        Ok(Ok(tiles)) => tiles.len(),
        Ok(Err(_)) | Err(_) => 0,
    };
    let mut retries = 0;
    let mut source_cancellations = 0;
    let mut results = if control.cancellation().is_cancelled() {
        source_cancellations = 1;
        cancelled_tiles(keys)
    } else {
        match outcome {
            Ok(Ok(tiles)) if tiles.len() == requests.len() => successful_tiles(keys, tiles),
            Ok(Ok(tiles)) if requests.len() > 1 => {
                let recovery = retry_tiles_individually(
                    source,
                    keys,
                    requests,
                    read_mode,
                    control,
                    format!(
                        "tile batch returned {} tiles for {} requests",
                        tiles.len(),
                        requests.len()
                    ),
                );
                retries = recovery.retries;
                source_cancellations = recovery.source_cancellations;
                recovery.results
            }
            Ok(Err(TileSourceError::Cancelled)) => {
                source_cancellations = 1;
                cancelled_tiles(keys)
            }
            Ok(Err(TileSourceError::Failed(error)))
                if requests.len() > 1 && !control.cancellation().is_cancelled() =>
            {
                let recovery =
                    retry_tiles_individually(source, keys, requests, read_mode, control, error);
                retries = recovery.retries;
                source_cancellations = recovery.source_cancellations;
                recovery.results
            }
            Ok(Ok(tiles)) => batch_failures(
                keys,
                format!(
                    "tile read returned {} tiles for {} requests",
                    tiles.len(),
                    requests.len()
                ),
            ),
            Ok(Err(TileSourceError::Failed(error))) => {
                batch_failures(keys, format!("tile read failed: {error}"))
            }
            Err(_) => batch_failures(keys, "tile decoder panicked".into()),
        }
    };
    if source.device_was_preferred(read_mode) {
        for result in &mut results {
            result.used_cpu_fallback = match &result.outcome {
                TileLoadOutcome::Decoded(tile) => tile.is_cpu(),
                TileLoadOutcome::Cancelled | TileLoadOutcome::Failed(_) => false,
            };
        }
    }
    TileDecodeBatch {
        results,
        admitted: requests.len(),
        returned,
        retries,
        source_cancellations,
    }
}

struct TileRecovery {
    results: Vec<TileLoadResult>,
    retries: usize,
    source_cancellations: usize,
}

fn retry_tiles_individually(
    source: &dyn TileSource,
    keys: Vec<TileKey>,
    requests: &[(dicom_viewer_core::LevelIndex, dicom_viewer_core::TileCoord)],
    read_mode: TileReadMode,
    control: &ReadControl,
    batch_error: String,
) -> TileRecovery {
    let mut cancellation_seen = false;
    let mut retries = 0;
    let mut source_cancellations = 0;
    let results = keys
        .into_iter()
        .zip(requests)
        .map(|(key, request)| {
            if cancellation_seen || control.cancellation().is_cancelled() {
                return TileLoadResult {
                    key,
                    outcome: TileLoadOutcome::Cancelled,
                    used_cpu_fallback: false,
                };
            }
            retries += 1;
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                source.read_tiles(std::slice::from_ref(request), read_mode, control)
            }));
            if control.cancellation().is_cancelled() {
                cancellation_seen = true;
                source_cancellations += 1;
                return TileLoadResult {
                    key,
                    outcome: TileLoadOutcome::Cancelled,
                    used_cpu_fallback: false,
                };
            }
            let result = match outcome {
                Ok(Ok(mut tiles)) if tiles.len() == 1 => Ok(tiles.remove(0)),
                Ok(Ok(tiles)) => Err(format!(
                    "tile batch recovery after {batch_error} returned {} tiles for one request",
                    tiles.len()
                )),
                Ok(Err(TileSourceError::Cancelled)) => {
                    cancellation_seen = true;
                    source_cancellations += 1;
                    return TileLoadResult {
                        key,
                        outcome: TileLoadOutcome::Cancelled,
                        used_cpu_fallback: false,
                    };
                }
                Ok(Err(TileSourceError::Failed(error))) => Err(format!(
                    "tile batch recovery after {batch_error} failed: {error}"
                )),
                Err(_) => Err(format!(
                    "tile decoder panicked during recovery after {batch_error}"
                )),
            };
            TileLoadResult {
                key,
                outcome: match result {
                    Ok(tile) => TileLoadOutcome::Decoded(tile),
                    Err(error) => TileLoadOutcome::Failed(TileFailure::new(error)),
                },
                used_cpu_fallback: false,
            }
        })
        .collect();
    TileRecovery {
        results,
        retries,
        source_cancellations,
    }
}

fn successful_tiles(keys: Vec<TileKey>, tiles: Vec<DecodedTile>) -> Vec<TileLoadResult> {
    keys.into_iter()
        .zip(tiles)
        .map(|(key, tile)| TileLoadResult {
            key,
            outcome: TileLoadOutcome::Decoded(tile),
            used_cpu_fallback: false,
        })
        .collect()
}

fn batch_failures(keys: Vec<TileKey>, error: String) -> Vec<TileLoadResult> {
    keys.into_iter()
        .map(|key| TileLoadResult {
            key,
            outcome: TileLoadOutcome::Failed(TileFailure::new(error.clone())),
            used_cpu_fallback: false,
        })
        .collect()
}

fn cancelled_tiles(keys: Vec<TileKey>) -> Vec<TileLoadResult> {
    keys.into_iter()
        .map(|key| TileLoadResult {
            key,
            outcome: TileLoadOutcome::Cancelled,
            used_cpu_fallback: false,
        })
        .collect()
}

fn finish_batch(shared: &LoaderShared, batch_id: u64, keys: &[TileKey]) -> Vec<TileKey> {
    let mut state = lock_state(shared);
    state.in_flight.remove(&batch_id);
    let mut current_keys = Vec::with_capacity(keys.len());
    for key in keys {
        if state.decoding.get(key) == Some(&batch_id) {
            state.decoding.remove(key);
            current_keys.push(*key);
        }
    }
    drop(state);
    shared.available.notify_one();
    current_keys
}

fn cancel_in_flight(state: &LoaderState) -> u64 {
    let mut cancelled = 0;
    for batch in state.in_flight.values() {
        if !batch.token.is_cancelled() {
            batch.token.cancel();
            cancelled += 1;
        }
    }
    cancelled
}

fn lock_state(shared: &LoaderShared) -> std::sync::MutexGuard<'_, LoaderState> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn default_worker_count() -> usize {
    let available = std::thread::available_parallelism().map_or(1, |count| count.get());
    configured_worker_count(
        std::env::var("DICOM_VIEWER_TILE_WORKERS").ok().as_deref(),
        available,
    )
}

fn configured_worker_count(value: Option<&str>, available: usize) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| *count > 0)
        .map_or_else(
            || available.saturating_sub(1).clamp(1, 4),
            |count| count.min(available.max(1)),
        )
}

fn configured_interactive_batch_size(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|size| matches!(size, 2 | 4 | 8))
        .unwrap_or(DEFAULT_INTERACTIVE_TILE_DECODE_BATCH_SIZE)
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::time::Duration;

    use dicom_viewer_core::{
        DicomIndexDiagnostic, DicomIndexMapping, DicomIndexOutcome, LevelIndex, ReadDiagnostic,
        TileCoord,
    };

    use super::super::QueueLane;
    use super::*;

    #[test]
    fn worker_count_override_supports_the_benchmark_matrix_without_changing_defaults() {
        assert_eq!(configured_worker_count(None, 12), 4);
        assert_eq!(configured_worker_count(Some("1"), 12), 1);
        assert_eq!(configured_worker_count(Some("2"), 12), 2);
        assert_eq!(configured_worker_count(Some("4"), 12), 4);
        assert_eq!(configured_worker_count(Some("invalid"), 12), 4);
    }

    #[test]
    fn interactive_batch_size_override_accepts_only_benchmark_candidates() {
        assert_eq!(configured_interactive_batch_size(None), 2);
        assert_eq!(configured_interactive_batch_size(Some("2")), 2);
        assert_eq!(configured_interactive_batch_size(Some("4")), 4);
        assert_eq!(configured_interactive_batch_size(Some("8")), 8);
        assert_eq!(configured_interactive_batch_size(Some("1")), 2);
        assert_eq!(configured_interactive_batch_size(Some("16")), 2);
        assert_eq!(configured_interactive_batch_size(Some("invalid")), 2);
    }

    fn study() -> Arc<ViewerStudy> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slide.j2k");
        std::fs::write(&path, j2k_test_support::htj2k_rgb8_fixture(16, 16)).unwrap();
        Arc::new(ViewerStudy::open_path(path).unwrap())
    }

    fn key(col: u64) -> TileKey {
        TileKey {
            generation: 1,
            level: LevelIndex::from_u32(0),
            coord: TileCoord::new(col, 0),
        }
    }

    fn priority(lane: QueueLane, distance2: u128, sequence: u64) -> TilePriority {
        TilePriority {
            lane,
            distance2,
            sequence,
        }
    }

    #[test]
    fn queue_reprioritizes_without_duplicating_current_work() {
        let loader = TileLoader::with_worker_count(0);
        let study = study();
        let tile = key(0);

        assert_eq!(
            loader
                .enqueue_or_reprioritize_batch(vec![QueuedTileRequest {
                    study: Arc::clone(&study),
                    key: tile,
                    priority: priority(QueueLane::Prefetch, 9, 0),
                    read_mode: TileReadMode::Preferred,
                }])
                .inserted,
            1
        );
        assert_eq!(
            loader
                .enqueue_or_reprioritize_batch(vec![QueuedTileRequest {
                    study,
                    key: tile,
                    priority: priority(QueueLane::Fallback, 1, 1),
                    read_mode: TileReadMode::Preferred,
                }])
                .inserted,
            0
        );

        let state = lock_state(&loader.shared);
        assert_eq!(state.queued.len(), 1);
        assert_eq!(state.queued[&tile].priority.lane, QueueLane::Fallback);
    }

    #[test]
    fn batches_are_priority_ordered_and_limited_to_eight() {
        let shared_study = study();
        let mut state = LoaderState::default();
        for col in 0..10 {
            let tile = key(col);
            let tile_priority = priority(QueueLane::Visible, u128::from(col), col);
            state.queued.insert(
                tile,
                QueuedJob {
                    priority: tile_priority,
                    read_mode: TileReadMode::Preferred,
                    enqueued_at: Some(Instant::now()),
                },
            );
            state.jobs.push(Reverse(TileJob {
                key: tile,
                priority: tile_priority,
                study: Arc::clone(&shared_study),
                read_mode: TileReadMode::Preferred,
                enqueued_at: Some(Instant::now()),
            }));
        }

        let batch = pop_next_batch(&mut state).unwrap();

        assert_eq!(batch.len(), 8);
        assert_eq!(batch[0].key, key(0));
        assert_eq!(batch[7].key, key(7));
    }

    #[test]
    fn interactive_mode_limits_inflight_batches_to_two_tiles() {
        let loader = TileLoader::with_worker_count(0);
        loader.set_interactive(true, false);
        let shared_study = study();
        let mut state = lock_state(&loader.shared);
        for col in 0..8 {
            let tile = key(col);
            let tile_priority = priority(QueueLane::Visible, u128::from(col), col);
            state.queued.insert(
                tile,
                QueuedJob {
                    priority: tile_priority,
                    read_mode: TileReadMode::Preferred,
                    enqueued_at: Some(Instant::now()),
                },
            );
            state.jobs.push(Reverse(TileJob {
                key: tile,
                priority: tile_priority,
                study: Arc::clone(&shared_study),
                read_mode: TileReadMode::Preferred,
                enqueued_at: Some(Instant::now()),
            }));
        }

        let batch = pop_next_batch(&mut state).unwrap();

        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].key, key(0));
        assert_eq!(batch[1].key, key(1));
    }

    #[test]
    fn configured_interactive_batch_size_controls_cpu_batch_admission() {
        let loader = TileLoader::with_worker_count(0);
        lock_state(&loader.shared).interactive_batch_size =
            configured_interactive_batch_size(Some("4"));
        loader.set_interactive(true, false);
        let shared_study = study();
        let requests = (0..8)
            .map(|col| request(&shared_study, col, QueueLane::Visible, col))
            .collect::<Vec<_>>();
        loader.enqueue_or_reprioritize_batch(requests);

        let batch = pop_next_batch(&mut lock_state(&loader.shared)).unwrap();

        assert_eq!(batch.len(), 4);
    }

    #[test]
    fn interactive_device_studies_keep_batches_of_eight() {
        let loader = TileLoader::with_worker_count(0);
        loader.set_interactive(true, true);
        let shared_study = study();
        let mut state = lock_state(&loader.shared);
        for col in 0..8 {
            let tile = key(col);
            let tile_priority = priority(QueueLane::Visible, u128::from(col), col);
            state.queued.insert(
                tile,
                QueuedJob {
                    priority: tile_priority,
                    read_mode: TileReadMode::Preferred,
                    enqueued_at: Some(Instant::now()),
                },
            );
            state.jobs.push(Reverse(TileJob {
                key: tile,
                priority: tile_priority,
                study: Arc::clone(&shared_study),
                read_mode: TileReadMode::Preferred,
                enqueued_at: Some(Instant::now()),
            }));
        }

        let batch = pop_next_batch(&mut state).unwrap();

        assert_eq!(batch.len(), 8);
    }

    #[test]
    fn bulk_enqueue_publishes_a_complete_coherent_batch_under_one_lock() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let requests = (0..8)
            .map(|col| QueuedTileRequest {
                study: Arc::clone(&shared_study),
                key: key(col),
                priority: priority(QueueLane::Visible, u128::from(col), col),
                read_mode: TileReadMode::Preferred,
            })
            .collect();

        assert_eq!(loader.enqueue_or_reprioritize_batch(requests).inserted, 8);
        let mut state = lock_state(&loader.shared);
        let batch = pop_next_batch(&mut state).unwrap();

        assert_eq!(batch.len(), 8);
        assert!(state.queued.is_empty());
    }

    #[test]
    fn visible_work_displaces_background_prefetch_at_the_queue_limit() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let prefetch = (0..MAX_QUEUED_TILES as u64)
            .map(|col| QueuedTileRequest {
                study: Arc::clone(&shared_study),
                key: key(col),
                priority: priority(QueueLane::Prefetch, u128::from(col), col),
                read_mode: TileReadMode::Preferred,
            })
            .collect();
        loader.enqueue_or_reprioritize_batch(prefetch);
        let visible = key(MAX_QUEUED_TILES as u64 + 1);

        loader.enqueue_or_reprioritize_batch(vec![QueuedTileRequest {
            study: shared_study,
            key: visible,
            priority: priority(QueueLane::Visible, 0, MAX_QUEUED_TILES as u64 + 1),
            read_mode: TileReadMode::Preferred,
        }]);

        let state = lock_state(&loader.shared);
        assert_eq!(state.queued.len(), MAX_QUEUED_TILES);
        assert!(state.queued.contains_key(&visible));
        assert_eq!(
            state
                .queued
                .values()
                .filter(|job| job.priority.lane == QueueLane::Prefetch)
                .count(),
            MAX_QUEUED_TILES - 1
        );
    }

    #[test]
    fn clearing_work_cancels_every_inflight_batch_token() {
        let loader = TileLoader::with_worker_count(0);
        let token = ReadCancellationToken::default();
        {
            let mut state = lock_state(&loader.shared);
            state.in_flight.insert(
                7,
                InFlightBatch {
                    demand_epoch: DemandEpoch(3),
                    lane: QueueLane::Visible,
                    keys: vec![key(0)],
                    token: token.clone(),
                },
            );
        }

        loader.clear_queued();

        assert!(token.is_cancelled());
    }

    #[test]
    fn preferred_reprioritization_preserves_cpu_mode_and_upgrades_priority() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let tile = key(0);
        let initial = loader.enqueue_or_reprioritize_batch(vec![QueuedTileRequest {
            study: Arc::clone(&shared_study),
            key: tile,
            priority: priority(QueueLane::Prefetch, 10, 0),
            read_mode: TileReadMode::CpuFallback,
        }]);

        let reprioritized = loader.enqueue_or_reprioritize_batch(vec![QueuedTileRequest {
            study: shared_study,
            key: tile,
            priority: priority(QueueLane::Visible, 0, 1),
            read_mode: TileReadMode::Preferred,
        }]);

        assert_eq!(initial.inserted, 1);
        assert_eq!(initial.deduplicated, 0);
        assert_eq!(reprioritized.inserted, 0);
        assert_eq!(reprioritized.deduplicated, 1);
        let state = lock_state(&loader.shared);
        assert_eq!(state.queued[&tile].read_mode, TileReadMode::CpuFallback);
        assert_eq!(state.queued[&tile].priority.lane, QueueLane::Visible);
    }

    fn request(
        shared_study: &Arc<ViewerStudy>,
        col: u64,
        lane: QueueLane,
        sequence: u64,
    ) -> QueuedTileRequest {
        QueuedTileRequest {
            study: Arc::clone(shared_study),
            key: key(col),
            priority: priority(lane, u128::from(col), sequence),
            read_mode: TileReadMode::Preferred,
        }
    }

    #[test]
    fn atomic_frame_demand_dispatches_visible_then_transition_before_background() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let requests = vec![
            request(&shared_study, 4, QueueLane::Prefetch, 4),
            request(&shared_study, 3, QueueLane::Overview, 3),
            request(&shared_study, 2, QueueLane::Fallback, 2),
            request(&shared_study, 1, QueueLane::TransitionTarget, 1),
            request(&shared_study, 0, QueueLane::Visible, 0),
        ];
        let keep = requests.iter().map(|request| request.key).collect();

        loader.publish_frame_demand(DemandEpoch(1), requests, &keep);

        let visible = wait_for_batch(&loader.shared).unwrap();
        assert_eq!(visible.jobs[0].priority.lane, QueueLane::Visible);
        let transition = wait_for_batch(&loader.shared).unwrap();
        assert_eq!(
            transition.jobs[0].priority.lane,
            QueueLane::TransitionTarget
        );
        let fallback = wait_for_batch(&loader.shared).unwrap();
        assert_eq!(fallback.jobs[0].priority.lane, QueueLane::Fallback);
        finish_batch(&loader.shared, visible.id, &[key(0)]);
        finish_batch(&loader.shared, transition.id, &[key(1)]);
        finish_batch(&loader.shared, fallback.id, &[key(2)]);
    }

    #[test]
    fn one_worker_alternates_visible_and_transition_across_demand_epochs() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let first_requests = vec![
            request(&shared_study, 0, QueueLane::Visible, 0),
            request(&shared_study, 1, QueueLane::TransitionTarget, 1),
        ];
        let first_keep = first_requests.iter().map(|request| request.key).collect();
        loader.publish_frame_demand(DemandEpoch(1), first_requests, &first_keep);

        let visible = wait_for_batch(&loader.shared).unwrap();
        assert_eq!(visible.jobs[0].priority.lane, QueueLane::Visible);
        finish_batch(&loader.shared, visible.id, &[key(0)]);

        let next_requests = vec![
            request(&shared_study, 2, QueueLane::Visible, 2),
            request(&shared_study, 1, QueueLane::TransitionTarget, 3),
        ];
        let next_keep = next_requests.iter().map(|request| request.key).collect();
        loader.publish_frame_demand(DemandEpoch(2), next_requests, &next_keep);

        let transition = wait_for_batch(&loader.shared).unwrap();
        assert_eq!(
            transition.jobs[0].priority.lane,
            QueueLane::TransitionTarget,
            "continuously refreshed visible demand must not reset one-worker foreground alternation"
        );
        finish_batch(&loader.shared, transition.id, &[key(1)]);
    }

    #[test]
    fn disjoint_new_demand_cancels_inflight_but_overlap_preserves_it() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let first_key = key(0);
        loader.publish_frame_demand(
            DemandEpoch(1),
            vec![request(&shared_study, 0, QueueLane::Visible, 0)],
            &HashSet::from([first_key]),
        );
        let batch = wait_for_batch(&loader.shared).unwrap();
        let token = batch.control.cancellation().clone();

        loader.publish_frame_demand(DemandEpoch(2), Vec::new(), &HashSet::from([first_key]));
        assert!(
            !token.is_cancelled(),
            "overlapping demand must preserve useful work"
        );

        loader.publish_frame_demand(
            DemandEpoch(3),
            vec![request(&shared_study, 1, QueueLane::Visible, 1)],
            &HashSet::from([key(1)]),
        );
        assert!(
            token.is_cancelled(),
            "disjoint demand must cancel obsolete work"
        );
        assert_eq!(loader.stats().cancellations, 1);
        finish_batch(&loader.shared, batch.id, &[first_key]);
    }

    #[test]
    fn rapid_a_b_a_dispatches_replacement_while_cancelled_a_is_still_held() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let a = key(0);
        let b = key(1);
        loader.publish_frame_demand(
            DemandEpoch(1),
            vec![request(&shared_study, 0, QueueLane::Visible, 0)],
            &HashSet::from([a]),
        );
        let held_a = wait_for_batch(&loader.shared).unwrap();

        loader.publish_frame_demand(
            DemandEpoch(2),
            vec![request(&shared_study, 1, QueueLane::Visible, 1)],
            &HashSet::from([b]),
        );
        assert!(held_a.control.cancellation().is_cancelled());
        loader.publish_frame_demand(
            DemandEpoch(3),
            vec![request(&shared_study, 0, QueueLane::Visible, 2)],
            &HashSet::from([a]),
        );

        assert!(
            lock_state(&loader.shared).queued.contains_key(&a),
            "re-demanded A must not deduplicate against its cancelled predecessor"
        );
        let replacement_a = wait_for_batch(&loader.shared).unwrap();
        assert_eq!(replacement_a.jobs[0].key, a);
        assert_ne!(replacement_a.id, held_a.id);

        finish_batch(&loader.shared, held_a.id, &[a]);
        assert_eq!(
            lock_state(&loader.shared).decoding.get(&a),
            Some(&replacement_a.id),
            "the obsolete A completion must not clear replacement A state"
        );
        finish_batch(&loader.shared, replacement_a.id, &[a]);
    }

    #[test]
    fn rapid_transition_a_b_a_ignores_cancelled_transition_inflight_guard() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let a = key(0);
        let b = key(1);
        loader.publish_frame_demand(
            DemandEpoch(1),
            vec![request(&shared_study, 0, QueueLane::TransitionTarget, 0)],
            &HashSet::from([a]),
        );
        let held_a = wait_for_batch(&loader.shared).unwrap();

        loader.publish_frame_demand(
            DemandEpoch(2),
            vec![request(&shared_study, 1, QueueLane::TransitionTarget, 1)],
            &HashSet::from([b]),
        );
        loader.publish_frame_demand(
            DemandEpoch(3),
            vec![request(&shared_study, 0, QueueLane::TransitionTarget, 2)],
            &HashSet::from([a]),
        );

        assert_eq!(
            choose_next_lane(&lock_state(&loader.shared)),
            Some(QueueLane::TransitionTarget),
            "a cancelled non-preemptive transition must not block replacement transition work"
        );
        let replacement_a = wait_for_batch(&loader.shared).unwrap();
        assert_eq!(replacement_a.jobs[0].key, a);
        finish_batch(&loader.shared, held_a.id, &[a]);
        finish_batch(&loader.shared, replacement_a.id, &[a]);
    }

    #[test]
    fn authoritative_frame_demand_can_demote_a_tile_that_left_the_view() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let tile = key(0);
        loader.publish_frame_demand(
            DemandEpoch(1),
            vec![request(&shared_study, 0, QueueLane::Visible, 0)],
            &HashSet::from([tile]),
        );
        loader.publish_frame_demand(
            DemandEpoch(2),
            vec![request(&shared_study, 0, QueueLane::Prefetch, 1)],
            &HashSet::from([tile]),
        );

        let state = lock_state(&loader.shared);
        assert_eq!(state.queued[&tile].priority.lane, QueueLane::Prefetch);
    }

    #[test]
    fn only_one_transition_batch_runs_alongside_visible_work() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let mut second_transition = request(&shared_study, 2, QueueLane::TransitionTarget, 2);
        second_transition.read_mode = TileReadMode::CpuFallback;
        let requests = vec![
            request(&shared_study, 0, QueueLane::Visible, 0),
            request(&shared_study, 1, QueueLane::TransitionTarget, 1),
            second_transition,
        ];
        let keep = requests.iter().map(|request| request.key).collect();
        loader.publish_frame_demand(DemandEpoch(1), requests, &keep);

        let visible = wait_for_batch(&loader.shared).unwrap();
        let transition = wait_for_batch(&loader.shared).unwrap();
        assert_eq!(visible.jobs[0].priority.lane, QueueLane::Visible);
        assert_eq!(
            transition.jobs[0].priority.lane,
            QueueLane::TransitionTarget
        );
        assert_eq!(choose_next_lane(&lock_state(&loader.shared)), None);

        finish_batch(&loader.shared, visible.id, &[key(0)]);
        assert_eq!(
            choose_next_lane(&lock_state(&loader.shared)),
            None,
            "finishing visible work must not admit a second transition batch while one remains in flight"
        );

        finish_batch(&loader.shared, transition.id, &[key(1)]);
        assert_eq!(
            choose_next_lane(&lock_state(&loader.shared)),
            Some(QueueLane::TransitionTarget),
            "the queued transition may run after the first transition batch finishes"
        );
    }

    struct BlockingTileSource {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl TileSource for BlockingTileSource {
        fn read_tiles(
            &self,
            requests: &[(LevelIndex, TileCoord)],
            _read_mode: TileReadMode,
            _control: &ReadControl,
        ) -> std::result::Result<Vec<DecodedTile>, TileSourceError> {
            self.entered.wait();
            self.release.wait();
            Ok(requests
                .iter()
                .map(|_| {
                    DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
                        width: 1,
                        height: 1,
                        rgba: vec![0, 0, 0, 255],
                    })
                })
                .collect())
        }

        fn device_was_preferred(&self, _read_mode: TileReadMode) -> bool {
            false
        }
    }

    #[test]
    fn cancellation_during_a_source_read_returns_only_silent_cancelled_outcomes() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let source = Arc::new(BlockingTileSource {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        let token = ReadCancellationToken::default();
        let control = ReadControl::new(token.clone());
        let thread = std::thread::spawn(move || {
            let requests = vec![(LevelIndex::from_u32(0), TileCoord::new(0, 0))];
            decode_source_requests(
                source.as_ref(),
                vec![key(0)],
                &requests,
                TileReadMode::Preferred,
                &control,
            )
        });
        entered.wait();
        token.cancel();
        release.wait();

        let results = thread.join().unwrap().results;
        assert!(results
            .iter()
            .all(|result| matches!(&result.outcome, TileLoadOutcome::Cancelled)));
    }

    #[derive(Clone, Copy)]
    enum FakeReadBehavior {
        FailTile(u64),
        WrongFirstCardinality,
        Cancel,
        Panic,
    }

    struct FakeTileSource {
        behavior: FakeReadBehavior,
        calls: Mutex<Vec<Vec<TileCoord>>>,
    }

    impl FakeTileSource {
        fn new(behavior: FakeReadBehavior) -> Self {
            Self {
                behavior,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl TileSource for FakeTileSource {
        fn read_tiles(
            &self,
            requests: &[(LevelIndex, TileCoord)],
            _read_mode: TileReadMode,
            _control: &ReadControl,
        ) -> std::result::Result<Vec<DecodedTile>, TileSourceError> {
            let mut calls = self.calls.lock().unwrap();
            calls.push(requests.iter().map(|(_, coord)| *coord).collect());
            let call_number = calls.len();
            drop(calls);
            match self.behavior {
                FakeReadBehavior::Panic => panic!("synthetic decoder panic"),
                FakeReadBehavior::Cancel => Err(TileSourceError::Cancelled),
                FakeReadBehavior::WrongFirstCardinality
                    if call_number == 1 && requests.len() > 1 =>
                {
                    Ok(Vec::new())
                }
                FakeReadBehavior::FailTile(col)
                    if requests.iter().any(|(_, coord)| coord.col() == col) =>
                {
                    Err(TileSourceError::Failed(format!("corrupt tile {col}")))
                }
                _ => Ok(requests
                    .iter()
                    .map(|_| {
                        DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
                            width: 1,
                            height: 1,
                            rgba: vec![0, 0, 0, 255],
                        })
                    })
                    .collect()),
            }
        }

        fn device_was_preferred(&self, _read_mode: TileReadMode) -> bool {
            false
        }
    }

    fn decode_with_fake(
        source: &dyn TileSource,
        columns: std::ops::Range<u64>,
    ) -> Vec<TileLoadResult> {
        let keys = columns.clone().map(key).collect::<Vec<_>>();
        let requests = columns
            .map(|col| (LevelIndex::from_u32(0), TileCoord::new(col, 0)))
            .collect::<Vec<_>>();
        decode_source_requests(
            source,
            keys,
            &requests,
            TileReadMode::Preferred,
            &ReadControl::default(),
        )
        .results
    }

    #[test]
    fn failed_batch_retries_each_tile_once_and_preserves_seven_valid_tiles() {
        let source = FakeTileSource::new(FakeReadBehavior::FailTile(3));

        let results = decode_with_fake(&source, 0..8);

        assert_eq!(source.call_count(), 9);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(&result.outcome, TileLoadOutcome::Decoded(_)))
                .count(),
            7
        );
        assert!(matches!(&results[3].outcome, TileLoadOutcome::Failed(_)));
    }

    #[test]
    fn wrong_batch_cardinality_isolated_by_single_tile_retries() {
        let source = FakeTileSource::new(FakeReadBehavior::WrongFirstCardinality);

        let results = decode_with_fake(&source, 0..3);

        assert_eq!(source.call_count(), 4);
        assert!(results
            .iter()
            .all(|result| matches!(&result.outcome, TileLoadOutcome::Decoded(_))));
    }

    #[test]
    fn decode_report_preserves_initial_cardinality_and_retry_counts() {
        let source = FakeTileSource::new(FakeReadBehavior::WrongFirstCardinality);
        let keys = (0..3).map(key).collect::<Vec<_>>();
        let requests = (0..3)
            .map(|col| (LevelIndex::from_u32(0), TileCoord::new(col, 0)))
            .collect::<Vec<_>>();

        let report = decode_source_requests(
            &source,
            keys,
            &requests,
            TileReadMode::Preferred,
            &ReadControl::default(),
        );

        assert_eq!(report.admitted, 3);
        assert_eq!(report.returned, 0);
        assert_eq!(report.retries, 3);
        assert_eq!(report.source_cancellations, 0);
        assert_eq!(report.results.len(), 3);
    }

    #[test]
    fn single_tile_failure_is_not_retried() {
        let source = FakeTileSource::new(FakeReadBehavior::FailTile(0));

        let results = decode_with_fake(&source, 0..1);

        assert_eq!(source.call_count(), 1);
        assert!(matches!(&results[0].outcome, TileLoadOutcome::Failed(_)));
    }

    #[test]
    fn typed_source_cancellation_is_never_retried_or_recorded_as_failure() {
        let source = FakeTileSource::new(FakeReadBehavior::Cancel);

        let results = decode_with_fake(&source, 0..8);

        assert_eq!(source.call_count(), 1);
        assert!(results
            .iter()
            .all(|result| matches!(&result.outcome, TileLoadOutcome::Cancelled)));
    }

    #[test]
    fn decoder_panic_does_not_create_a_retry_storm() {
        let source = FakeTileSource::new(FakeReadBehavior::Panic);

        let results = decode_with_fake(&source, 0..8);

        assert_eq!(source.call_count(), 1);
        assert!(results.iter().all(|result| {
            matches!(&result.outcome, TileLoadOutcome::Failed(failure) if failure.message.contains("panicked"))
        }));
    }

    #[test]
    fn idle_workers_shutdown_cleanly() {
        let loader = TileLoader::with_worker_count(2);
        drop(loader);
    }

    #[test]
    fn completed_keys_remain_inflight_until_the_ui_acknowledges_delivery() {
        let loader = TileLoader::with_worker_count(1);
        let shared_study = study();
        let tile = key(0);
        loader.enqueue_or_reprioritize_batch(vec![request(
            &shared_study,
            0,
            QueueLane::Visible,
            0,
        )]);
        let receiver = loader.receiver.as_ref().unwrap();
        assert!(matches!(
            receiver.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(TileLoaderMessage::Started { keys, .. }) if keys == vec![tile]
        ));
        let (batch_id, keys) = match receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
        {
            TileLoaderMessage::Finished {
                batch_id, results, ..
            } => (
                batch_id,
                results.iter().map(|result| result.key).collect::<Vec<_>>(),
            ),
            TileLoaderMessage::Started { .. } => panic!("worker sent two Started messages"),
        };
        assert!(lock_state(&loader.shared).decoding.contains_key(&tile));

        loader.acknowledge_finished(batch_id, &keys);

        assert!(!lock_state(&loader.shared).decoding.contains_key(&tile));
    }

    #[test]
    fn disabled_metrics_capture_no_enqueue_timestamps() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        loader.enqueue_or_reprioritize_batch(vec![request(
            &shared_study,
            0,
            QueueLane::Visible,
            0,
        )]);

        assert!(lock_state(&loader.shared).queued[&key(0)]
            .enqueued_at
            .is_none());
    }

    #[test]
    fn enabled_metrics_capture_enqueue_timestamps() {
        let loader = TileLoader::with_worker_count_and_metrics(0, true);
        let shared_study = study();
        loader.enqueue_or_reprioritize_batch(vec![request(
            &shared_study,
            0,
            QueueLane::Visible,
            0,
        )]);

        assert!(lock_state(&loader.shared).queued[&key(0)]
            .enqueued_at
            .is_some());
    }

    #[test]
    fn queue_wait_samples_preserve_each_tiles_lane_and_wait() {
        let observed_at = Instant::now();
        let shared_study = study();
        let jobs = vec![
            TileJob {
                key: key(0),
                priority: priority(QueueLane::Visible, 0, 0),
                study: Arc::clone(&shared_study),
                read_mode: TileReadMode::Preferred,
                enqueued_at: Some(observed_at - Duration::from_millis(100)),
            },
            TileJob {
                key: key(1),
                priority: priority(QueueLane::TransitionTarget, 0, 1),
                study: shared_study,
                read_mode: TileReadMode::Preferred,
                enqueued_at: Some(observed_at - Duration::from_millis(4)),
            },
        ];

        let samples = queue_wait_samples(&jobs, observed_at);

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].lane, QueueLane::Visible);
        assert_eq!(samples[0].milliseconds, 100.0);
        assert_eq!(samples[1].lane, QueueLane::TransitionTarget);
        assert_eq!(samples[1].milliseconds, 4.0);
    }

    #[test]
    fn loader_batch_collects_read_index_diagnostics_only_when_metrics_are_enabled() {
        let diagnostic = DicomIndexDiagnostic::new(
            DicomIndexOutcome::BuiltFast {
                mapping: DicomIndexMapping::BasicOffsetTableItems,
            },
            std::time::Duration::from_millis(9),
        );
        for (metrics_enabled, expected) in [(false, Vec::new()), (true, vec![diagnostic])] {
            let loader = TileLoader::with_worker_count_and_metrics(0, metrics_enabled);
            let shared_study = study();
            loader.enqueue_or_reprioritize_batch(vec![request(
                &shared_study,
                0,
                QueueLane::Visible,
                0,
            )]);
            let batch = wait_for_batch(&loader.shared).expect("queued tile should form a batch");
            batch
                .control
                .record_diagnostic(ReadDiagnostic::DicomIndex(diagnostic));

            assert_eq!(take_index_diagnostics(&batch.index_diagnostics), expected);
            finish_batch(&loader.shared, batch.id, &[key(0)]);
        }
    }

    #[test]
    fn loader_statistics_report_each_queued_lane() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        loader.enqueue_or_reprioritize_batch(vec![
            request(&shared_study, 0, QueueLane::Visible, 0),
            request(&shared_study, 1, QueueLane::TransitionTarget, 1),
            request(&shared_study, 2, QueueLane::Fallback, 2),
            request(&shared_study, 3, QueueLane::Overview, 3),
            request(&shared_study, 4, QueueLane::Prefetch, 4),
        ]);

        let stats = loader.stats();
        assert_eq!(stats.queued, 5);
        assert_eq!(stats.visible, 1);
        assert_eq!(stats.transition, 1);
        assert_eq!(stats.fallback, 1);
        assert_eq!(stats.overview, 1);
        assert_eq!(stats.prefetch, 1);
    }
}
