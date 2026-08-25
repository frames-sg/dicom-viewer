mod decode;
mod queue;
mod worker;

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    mpsc::{self, Receiver, SyncSender},
    Arc, Condvar, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use dicom_viewer_core::{DicomIndexDiagnostic, ReadCancellationToken, ReadControl, ViewerStudy};

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
    max_in_flight_decoded_bytes: usize,
    in_flight_decoded_bytes: usize,
    interactive_batch_size: usize,
    metrics_enabled: bool,
    cancellations: u64,
    shutdown: bool,
}

impl LoaderState {
    fn new(metrics_enabled: bool, max_in_flight_decoded_bytes: usize) -> Self {
        Self {
            jobs: BinaryHeap::new(),
            queued: HashMap::new(),
            decoding: HashMap::new(),
            in_flight: HashMap::new(),
            next_batch_id: 0,
            demand_epoch: DemandEpoch::INITIAL,
            last_foreground_lane: None,
            max_batch_size: TILE_DECODE_BATCH_SIZE,
            max_in_flight_decoded_bytes: max_in_flight_decoded_bytes.max(1),
            in_flight_decoded_bytes: 0,
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
        Self::new(false, usize::MAX)
    }
}

struct InFlightBatch {
    demand_epoch: DemandEpoch,
    lane: QueueLane,
    keys: Vec<TileKey>,
    reserved_decoded_bytes: usize,
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
    pub(super) fn new(metrics_enabled: bool, max_in_flight_decoded_bytes: usize) -> Self {
        Self::with_worker_count_and_metrics(
            default_worker_count(),
            metrics_enabled,
            max_in_flight_decoded_bytes,
        )
    }

    #[cfg(test)]
    fn with_worker_count(worker_count: usize) -> Self {
        Self::with_worker_count_and_metrics(worker_count, false, usize::MAX)
    }

    fn with_worker_count_and_metrics(
        worker_count: usize,
        metrics_enabled: bool,
        max_in_flight_decoded_bytes: usize,
    ) -> Self {
        let shared = Arc::new(LoaderShared {
            state: Mutex::new(LoaderState::new(
                metrics_enabled,
                max_in_flight_decoded_bytes,
            )),
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

    pub(super) fn current_keys_for_batch(&self, batch_id: u64, keys: &[TileKey]) -> Vec<TileKey> {
        let state = lock_state(&self.shared);
        keys.iter()
            .filter(|key| state.decoding.get(key) == Some(&batch_id))
            .copied()
            .collect()
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

use queue::{cancel_in_flight, enqueue_requests, finish_batch, lock_state};
use worker::spawn_worker;

#[cfg(test)]
use decode::{decode_source_requests, TileSource, TileSourceError};
#[cfg(test)]
use queue::{choose_next_lane, pop_next_batch, take_index_diagnostics, wait_for_batch};
#[cfg(test)]
use worker::queue_wait_samples;

#[cfg(test)]
mod tests;
