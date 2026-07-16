use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc, Condvar, Mutex,
};
use std::thread::{self, JoinHandle};

use dicom_viewer_core::ViewerStudy;

use super::{DecodedTile, TileKey, TileLoadResult, TilePriority};

const TILE_DECODE_BATCH_SIZE: usize = 8;
const INTERACTIVE_TILE_DECODE_BATCH_SIZE: usize = 2;

pub(super) struct TileLoader {
    shared: Arc<LoaderShared>,
    receiver: Receiver<TileLoaderMessage>,
    workers: Vec<JoinHandle<()>>,
    pub(super) startup_error: Option<String>,
    sequence: u64,
}

pub(super) enum TileLoaderMessage {
    Started(Vec<TileKey>),
    Finished(Vec<TileLoadResult>),
}

struct LoaderShared {
    state: Mutex<LoaderState>,
    available: Condvar,
}

struct LoaderState {
    jobs: BinaryHeap<Reverse<TileJob>>,
    queued: HashMap<TileKey, QueuedJob>,
    decoding: HashSet<TileKey>,
    max_batch_size: usize,
    shutdown: bool,
}

impl Default for LoaderState {
    fn default() -> Self {
        Self {
            jobs: BinaryHeap::new(),
            queued: HashMap::new(),
            decoding: HashSet::new(),
            max_batch_size: TILE_DECODE_BATCH_SIZE,
            shutdown: false,
        }
    }
}

struct TileJob {
    key: TileKey,
    priority: TilePriority,
    study: Arc<ViewerStudy>,
    read_mode: TileReadMode,
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
}

pub(super) struct QueuedTileRequest {
    pub(super) study: Arc<ViewerStudy>,
    pub(super) key: TileKey,
    pub(super) priority: TilePriority,
    pub(super) read_mode: TileReadMode,
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
    pub(super) fn new() -> Self {
        Self::with_worker_count(default_worker_count())
    }

    fn with_worker_count(worker_count: usize) -> Self {
        let shared = Arc::new(LoaderShared {
            state: Mutex::new(LoaderState::default()),
            available: Condvar::new(),
        });
        let (sender, receiver) = mpsc::channel();
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
            receiver,
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
            INTERACTIVE_TILE_DECODE_BATCH_SIZE
        } else {
            TILE_DECODE_BATCH_SIZE
        };
    }

    pub(super) fn enqueue_or_reprioritize_batch(&self, requests: Vec<QueuedTileRequest>) -> usize {
        let mut state = lock_state(&self.shared);
        let mut inserted = 0;
        for request in requests {
            if state.decoding.contains(&request.key) {
                continue;
            }
            if let Some(current) = state.queued.get_mut(&request.key) {
                let keep_cpu_fallback = current.read_mode == TileReadMode::CpuFallback
                    && request.read_mode == TileReadMode::Preferred;
                if !keep_cpu_fallback && request.priority < current.priority {
                    *current = QueuedJob {
                        priority: request.priority,
                        read_mode: request.read_mode,
                    };
                    state.jobs.push(Reverse(TileJob {
                        key: request.key,
                        priority: request.priority,
                        study: request.study,
                        read_mode: request.read_mode,
                    }));
                }
                continue;
            }
            state.queued.insert(
                request.key,
                QueuedJob {
                    priority: request.priority,
                    read_mode: request.read_mode,
                },
            );
            state.jobs.push(Reverse(TileJob {
                key: request.key,
                priority: request.priority,
                study: request.study,
                read_mode: request.read_mode,
            }));
            inserted += 1;
        }
        drop(state);
        if inserted > 0 {
            self.shared.available.notify_all();
        } else {
            // Reprioritized work can still need to wake an idle worker.
            self.shared.available.notify_one();
        }
        inserted
    }

    pub(super) fn clear_queued(&self) {
        let mut state = lock_state(&self.shared);
        state.jobs.clear();
        state.queued.clear();
    }

    pub(super) fn retain_queued_tiles(&self, keep: &HashSet<TileKey>) {
        let mut state = lock_state(&self.shared);
        state.jobs.retain(|Reverse(job)| keep.contains(&job.key));
        state.queued.retain(|key, _| keep.contains(key));
    }

    pub(super) fn try_recv(&self) -> std::result::Result<TileLoaderMessage, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for TileLoader {
    fn drop(&mut self) {
        {
            let mut state = lock_state(&self.shared);
            state.shutdown = true;
            state.jobs.clear();
            state.queued.clear();
        }
        self.shared.available.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn spawn_worker(
    worker_index: usize,
    shared: Arc<LoaderShared>,
    sender: Sender<TileLoaderMessage>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("dicom-viewer-tile-{worker_index}"))
        .spawn(move || run_worker(&shared, &sender))
}

fn run_worker(shared: &LoaderShared, sender: &Sender<TileLoaderMessage>) {
    loop {
        let Some(jobs) = wait_for_batch(shared) else {
            return;
        };
        let keys = jobs.iter().map(|job| job.key).collect::<Vec<_>>();
        if sender
            .send(TileLoaderMessage::Started(keys.clone()))
            .is_err()
        {
            finish_batch(shared, &keys);
            return;
        }

        let results = decode_tile_jobs(jobs);
        finish_batch(shared, &keys);
        if sender.send(TileLoaderMessage::Finished(results)).is_err() {
            return;
        }
    }
}

fn wait_for_batch(shared: &LoaderShared) -> Option<Vec<TileJob>> {
    let mut state = lock_state(shared);
    loop {
        if state.shutdown {
            return None;
        }
        if let Some(batch) = pop_next_batch(&mut state) {
            return Some(batch);
        }
        state = shared
            .available
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

fn pop_next_batch(state: &mut LoaderState) -> Option<Vec<TileJob>> {
    let first = pop_next_valid_job(state)?;
    let mut jobs = vec![first];
    while jobs.len() < state.max_batch_size {
        match state.jobs.peek() {
            Some(Reverse(job)) if !is_current_job(state, job) => {
                state.jobs.pop();
            }
            Some(Reverse(job)) if can_batch(&jobs[0], job) => {
                if let Some(job) = pop_next_valid_job(state) {
                    jobs.push(job);
                }
            }
            _ => break,
        }
    }
    Some(jobs)
}

fn pop_next_valid_job(state: &mut LoaderState) -> Option<TileJob> {
    while let Some(Reverse(job)) = state.jobs.pop() {
        if is_current_job(state, &job) {
            state.queued.remove(&job.key);
            state.decoding.insert(job.key);
            return Some(job);
        }
    }
    None
}

fn is_current_job(state: &LoaderState, job: &TileJob) -> bool {
    state.queued.get(&job.key)
        == Some(&QueuedJob {
            priority: job.priority,
            read_mode: job.read_mode,
        })
}

fn can_batch(first: &TileJob, next: &TileJob) -> bool {
    first.key.generation == next.key.generation
        && first.key.level == next.key.level
        && first.priority.lane == next.priority.lane
        && first.read_mode == next.read_mode
        && Arc::ptr_eq(&first.study, &next.study)
}

fn decode_tile_jobs(jobs: Vec<TileJob>) -> Vec<TileLoadResult> {
    let keys = jobs.iter().map(|job| job.key).collect::<Vec<_>>();
    let requests = jobs
        .iter()
        .map(|job| (job.key.level, job.key.coord))
        .collect::<Vec<_>>();
    let read_mode = jobs[0].read_mode;
    let outcome = catch_unwind(AssertUnwindSafe(|| match read_mode {
        TileReadMode::Preferred => jobs[0]
            .study
            .read_tiles_for_render(&requests)
            .map(|tiles| {
                tiles
                    .into_iter()
                    .map(DecodedTile::from_render_tile)
                    .collect()
            })
            .map_err(|err| err.to_string()),
        TileReadMode::CpuFallback => jobs[0]
            .study
            .read_tiles_rgba(&requests)
            .map(|tiles| tiles.into_iter().map(DecodedTile::from_rgba_tile).collect())
            .map_err(|err| err.to_string()),
    }));
    let device_was_preferred = read_mode == TileReadMode::Preferred
        && jobs[0].study.summary().tile_decode_backend != dicom_viewer_core::TileDecodeBackend::Cpu;
    let mut results = map_batch_outcome(keys, jobs.len(), outcome);
    if device_was_preferred {
        for result in &mut results {
            result.used_cpu_fallback = result.result.as_ref().is_ok_and(DecodedTile::is_cpu);
        }
    }
    results
}

fn map_batch_outcome(
    keys: Vec<TileKey>,
    expected_tiles: usize,
    outcome: std::thread::Result<std::result::Result<Vec<DecodedTile>, String>>,
) -> Vec<TileLoadResult> {
    match outcome {
        Ok(Ok(tiles)) if tiles.len() == expected_tiles => keys
            .into_iter()
            .zip(tiles)
            .map(|(key, tile)| TileLoadResult {
                key,
                result: Ok(tile),
                used_cpu_fallback: false,
            })
            .collect(),
        Ok(Ok(tiles)) => batch_failures(
            keys,
            format!(
                "tile batch returned {} tiles for {expected_tiles} requests",
                tiles.len()
            ),
        ),
        Ok(Err(err)) => batch_failures(keys, format!("tile batch failed: {err}")),
        Err(_) => batch_failures(keys, "tile decoder panicked".into()),
    }
}

fn batch_failures(keys: Vec<TileKey>, error: String) -> Vec<TileLoadResult> {
    keys.into_iter()
        .map(|key| TileLoadResult {
            key,
            result: Err(error.clone()),
            used_cpu_fallback: false,
        })
        .collect()
}

fn finish_batch(shared: &LoaderShared, keys: &[TileKey]) {
    let mut state = lock_state(shared);
    for key in keys {
        state.decoding.remove(key);
    }
    drop(state);
    shared.available.notify_one();
}

fn lock_state(shared: &LoaderShared) -> std::sync::MutexGuard<'_, LoaderState> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn default_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().saturating_sub(1).clamp(1, 4))
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use dicom_viewer_core::{LevelIndex, TileCoord};

    use super::super::QueueLane;
    use super::*;

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
            loader.enqueue_or_reprioritize_batch(vec![QueuedTileRequest {
                study: Arc::clone(&study),
                key: tile,
                priority: priority(QueueLane::Prefetch, 9, 0),
                read_mode: TileReadMode::Preferred,
            }]),
            1
        );
        assert_eq!(
            loader.enqueue_or_reprioritize_batch(vec![QueuedTileRequest {
                study,
                key: tile,
                priority: priority(QueueLane::Fallback, 1, 1),
                read_mode: TileReadMode::Preferred,
            }]),
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
                },
            );
            state.jobs.push(Reverse(TileJob {
                key: tile,
                priority: tile_priority,
                study: Arc::clone(&shared_study),
                read_mode: TileReadMode::Preferred,
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
                },
            );
            state.jobs.push(Reverse(TileJob {
                key: tile,
                priority: tile_priority,
                study: Arc::clone(&shared_study),
                read_mode: TileReadMode::Preferred,
            }));
        }

        let batch = pop_next_batch(&mut state).unwrap();

        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].key, key(0));
        assert_eq!(batch[1].key, key(1));
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
                },
            );
            state.jobs.push(Reverse(TileJob {
                key: tile,
                priority: tile_priority,
                study: Arc::clone(&shared_study),
                read_mode: TileReadMode::Preferred,
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

        assert_eq!(loader.enqueue_or_reprioritize_batch(requests), 8);
        let mut state = lock_state(&loader.shared);
        let batch = pop_next_batch(&mut state).unwrap();

        assert_eq!(batch.len(), 8);
        assert!(state.queued.is_empty());
    }

    #[test]
    fn preferred_reprioritization_does_not_replace_a_queued_cpu_fallback() {
        let loader = TileLoader::with_worker_count(0);
        let shared_study = study();
        let tile = key(0);
        loader.enqueue_or_reprioritize_batch(vec![QueuedTileRequest {
            study: Arc::clone(&shared_study),
            key: tile,
            priority: priority(QueueLane::Visible, 10, 0),
            read_mode: TileReadMode::CpuFallback,
        }]);

        loader.enqueue_or_reprioritize_batch(vec![QueuedTileRequest {
            study: shared_study,
            key: tile,
            priority: priority(QueueLane::Fallback, 0, 1),
            read_mode: TileReadMode::Preferred,
        }]);

        let state = lock_state(&loader.shared);
        assert_eq!(state.queued[&tile].read_mode, TileReadMode::CpuFallback);
    }

    #[test]
    fn failed_batch_is_reported_for_every_tile_without_single_tile_retries() {
        let study = study();
        let first = key(99);
        let second = key(100);
        let results = decode_tile_jobs(vec![
            TileJob {
                key: first,
                priority: priority(QueueLane::Visible, 0, 0),
                study: Arc::clone(&study),
                read_mode: TileReadMode::Preferred,
            },
            TileJob {
                key: second,
                priority: priority(QueueLane::Visible, 1, 1),
                study,
                read_mode: TileReadMode::Preferred,
            },
        ]);

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.result.is_err()));
    }

    #[test]
    fn decoder_panic_becomes_a_failure_for_each_tile() {
        let keys = vec![key(0), key(1)];
        let outcome = catch_unwind(|| -> std::result::Result<Vec<DecodedTile>, String> {
            panic!("synthetic decoder panic")
        });

        let results = map_batch_outcome(keys, 2, outcome);

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| {
            matches!(&result.result, Err(message) if message.contains("panicked"))
        }));
    }

    #[test]
    fn idle_workers_shutdown_cleanly() {
        let loader = TileLoader::with_worker_count(2);
        drop(loader);
    }
}
