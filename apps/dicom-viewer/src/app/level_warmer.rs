use std::any::Any;
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dicom_viewer_core::{
    DicomIndexDiagnostic, LevelIndex, ReadCancellationToken, ReadControl, ViewerStudy,
};

const EVENT_CHANNEL_CAPACITY: usize = 16;

trait LevelPreparer: Send + Sync + 'static {
    fn prepare_level_controlled(
        &self,
        level: LevelIndex,
        control: &ReadControl,
    ) -> std::result::Result<(), String>;
}

impl LevelPreparer for ViewerStudy {
    fn prepare_level_controlled(
        &self,
        level: LevelIndex,
        control: &ReadControl,
    ) -> std::result::Result<(), String> {
        ViewerStudy::prepare_level_controlled(self, level, control)
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LevelWarmerEvent {
    Prepared {
        study_generation: u64,
        level: LevelIndex,
        elapsed: Duration,
        index_diagnostics: Vec<DicomIndexDiagnostic>,
    },
    Failed {
        study_generation: u64,
        level: LevelIndex,
        error: String,
        elapsed: Duration,
        index_diagnostics: Vec<DicomIndexDiagnostic>,
    },
    Cancelled {
        study_generation: u64,
        level: LevelIndex,
        elapsed: Duration,
        index_diagnostics: Vec<DicomIndexDiagnostic>,
    },
}

impl LevelWarmerEvent {
    pub(super) const fn study_generation(&self) -> u64 {
        match self {
            Self::Prepared {
                study_generation, ..
            }
            | Self::Failed {
                study_generation, ..
            }
            | Self::Cancelled {
                study_generation, ..
            } => *study_generation,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct LevelWarmerStats {
    pub(super) study_generation: Option<u64>,
    pub(super) active_study_generation: Option<u64>,
    pub(super) active_level: Option<LevelIndex>,
    pub(super) pending_levels: usize,
    pub(super) prepared_levels: usize,
    pub(super) failed_levels: usize,
    pub(super) prepared_total: u64,
    pub(super) failed_total: u64,
    pub(super) cancelled_total: u64,
    pub(super) events_dropped: u64,
}

pub(super) struct LevelWarmer {
    shared: Arc<Shared>,
    receiver: Option<Receiver<LevelWarmerEvent>>,
    worker: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<WarmerState>,
    available: Condvar,
}

#[derive(Default)]
struct WarmerState {
    plan: Option<WarmPlan>,
    active: Option<ActivePreparation>,
    next_plan_id: u64,
    metrics_generation: Option<u64>,
    prepared_total: u64,
    failed_total: u64,
    cancelled_total: u64,
    events_dropped: u64,
    shutdown: bool,
}

struct WarmPlan {
    id: u64,
    study_generation: u64,
    preparer: Arc<dyn LevelPreparer>,
    priorities: Vec<LevelIndex>,
    attempted: HashSet<LevelIndex>,
    prepared: HashSet<LevelIndex>,
    failed: HashSet<LevelIndex>,
    diagnostics_enabled: bool,
}

struct ActivePreparation {
    plan_id: u64,
    study_generation: u64,
    level: LevelIndex,
    token: ReadCancellationToken,
}

struct PreparationWork {
    plan_id: u64,
    study_generation: u64,
    level: LevelIndex,
    preparer: Arc<dyn LevelPreparer>,
    control: ReadControl,
    index_diagnostics: Option<Arc<Mutex<Vec<DicomIndexDiagnostic>>>>,
    diagnostics_enabled: bool,
}

impl LevelWarmer {
    pub(super) fn new() -> std::io::Result<Self> {
        Self::spawn(EVENT_CHANNEL_CAPACITY)
    }

    #[cfg(test)]
    fn with_event_capacity(capacity: usize) -> std::io::Result<Self> {
        Self::spawn(capacity)
    }

    fn spawn(event_capacity: usize) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(WarmerState::default()),
            available: Condvar::new(),
        });
        let (sender, receiver) = mpsc::sync_channel(event_capacity);
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("dicom-viewer-level-warmer".into())
            .spawn(move || run_worker(&worker_shared, &sender))?;
        Ok(Self {
            shared,
            receiver: Some(receiver),
            worker: Some(worker),
        })
    }

    pub(super) fn publish_study(
        &self,
        study: Arc<ViewerStudy>,
        study_generation: u64,
        priorities: Vec<LevelIndex>,
        diagnostics_enabled: bool,
    ) {
        let preparer: Arc<dyn LevelPreparer> = study;
        self.publish_dyn(preparer, study_generation, priorities, diagnostics_enabled);
    }

    #[cfg(test)]
    fn publish_preparer<P>(
        &self,
        preparer: Arc<P>,
        study_generation: u64,
        priorities: Vec<LevelIndex>,
    ) where
        P: LevelPreparer,
    {
        let preparer: Arc<dyn LevelPreparer> = preparer;
        self.publish_dyn(preparer, study_generation, priorities, false);
    }

    #[cfg(test)]
    fn publish_preparer_with_diagnostics<P>(
        &self,
        preparer: Arc<P>,
        study_generation: u64,
        priorities: Vec<LevelIndex>,
        diagnostics_enabled: bool,
    ) where
        P: LevelPreparer,
    {
        let preparer: Arc<dyn LevelPreparer> = preparer;
        self.publish_dyn(preparer, study_generation, priorities, diagnostics_enabled);
    }

    fn publish_dyn(
        &self,
        preparer: Arc<dyn LevelPreparer>,
        study_generation: u64,
        priorities: Vec<LevelIndex>,
        diagnostics_enabled: bool,
    ) {
        let priorities = deduplicate_levels(priorities);
        let mut state = lock_state(&self.shared);
        if state.metrics_generation != Some(study_generation) {
            state.metrics_generation = Some(study_generation);
            state.prepared_total = 0;
            state.failed_total = 0;
            state.cancelled_total = 0;
            state.events_dropped = 0;
        }
        let same_study = state.plan.as_ref().is_some_and(|plan| {
            plan.study_generation == study_generation && Arc::ptr_eq(&plan.preparer, &preparer)
        });
        if same_study {
            if let Some(plan) = state.plan.as_mut() {
                plan.priorities = priorities;
                plan.diagnostics_enabled = diagnostics_enabled;
            }
        } else {
            cancel_active(&state);
            let plan_id = state.next_plan_id;
            state.next_plan_id = state.next_plan_id.wrapping_add(1);
            state.plan = Some(WarmPlan {
                id: plan_id,
                study_generation,
                preparer,
                priorities,
                attempted: HashSet::new(),
                prepared: HashSet::new(),
                failed: HashSet::new(),
                diagnostics_enabled,
            });
        }
        drop(state);
        self.shared.available.notify_one();
    }

    pub(super) fn clear(&self) {
        let mut state = lock_state(&self.shared);
        cancel_active(&state);
        state.plan = None;
        state.metrics_generation = None;
        state.prepared_total = 0;
        state.failed_total = 0;
        state.cancelled_total = 0;
        state.events_dropped = 0;
        drop(state);
        self.shared.available.notify_one();
    }

    pub(super) fn try_recv(&self) -> std::result::Result<LevelWarmerEvent, mpsc::TryRecvError> {
        self.receiver
            .as_ref()
            .expect("level-warmer receiver must exist before drop")
            .try_recv()
    }

    pub(super) fn stats(&self) -> LevelWarmerStats {
        let state = lock_state(&self.shared);
        let (study_generation, pending_levels, prepared_levels, failed_levels) =
            state.plan.as_ref().map_or((None, 0, 0, 0), |plan| {
                (
                    Some(plan.study_generation),
                    plan.priorities
                        .iter()
                        .filter(|level| !plan.attempted.contains(level))
                        .count(),
                    plan.prepared.len(),
                    plan.failed.len(),
                )
            });
        LevelWarmerStats {
            study_generation,
            active_study_generation: state.active.as_ref().map(|active| active.study_generation),
            active_level: state.active.as_ref().map(|active| active.level),
            pending_levels,
            prepared_levels,
            failed_levels,
            prepared_total: state.prepared_total,
            failed_total: state.failed_total,
            cancelled_total: state.cancelled_total,
            events_dropped: state.events_dropped,
        }
    }
}

impl Drop for LevelWarmer {
    fn drop(&mut self) {
        {
            let mut state = lock_state(&self.shared);
            state.shutdown = true;
            cancel_active(&state);
            state.plan = None;
        }
        drop(self.receiver.take());
        self.shared.available.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(shared: &Shared, sender: &SyncSender<LevelWarmerEvent>) {
    while let Some(work) = wait_for_work(shared) {
        let started = diagnostic_timer_with(work.diagnostics_enabled, Instant::now);
        let result = catch_unwind(AssertUnwindSafe(|| {
            work.preparer
                .prepare_level_controlled(work.level, &work.control)
        }))
        .unwrap_or_else(|panic| {
            Err(format!(
                "level preparation panicked: {}",
                panic_message(panic)
            ))
        });
        let elapsed = started.map_or(Duration::ZERO, |started| started.elapsed());
        let event = finish_work(shared, &work, result, elapsed);
        emit_event(shared, sender, event);
    }
}

fn diagnostic_timer_with(enabled: bool, clock: impl FnOnce() -> Instant) -> Option<Instant> {
    enabled.then(clock)
}

fn wait_for_work(shared: &Shared) -> Option<PreparationWork> {
    let mut state = lock_state(shared);
    loop {
        if state.shutdown {
            return None;
        }
        if state.active.is_none() {
            let selected = state.plan.as_mut().and_then(|plan| {
                let level = plan
                    .priorities
                    .iter()
                    .copied()
                    .find(|level| !plan.attempted.contains(level))?;
                plan.attempted.insert(level);
                Some((
                    plan.id,
                    plan.study_generation,
                    level,
                    Arc::clone(&plan.preparer),
                    plan.diagnostics_enabled,
                ))
            });
            if let Some((plan_id, study_generation, level, preparer, diagnostics_enabled)) =
                selected
            {
                let token = ReadCancellationToken::new();
                let index_diagnostics = diagnostics_enabled
                    .then(|| Arc::new(Mutex::new(Vec::<DicomIndexDiagnostic>::new())));
                let mut control = ReadControl::new(token.clone());
                if let Some(diagnostics) = &index_diagnostics {
                    let captured = Arc::clone(diagnostics);
                    control = control.with_diagnostic_sink(Arc::new(move |diagnostic| {
                        captured
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(diagnostic);
                    }));
                }
                state.active = Some(ActivePreparation {
                    plan_id,
                    study_generation,
                    level,
                    token,
                });
                return Some(PreparationWork {
                    plan_id,
                    study_generation,
                    level,
                    preparer,
                    control,
                    index_diagnostics,
                    diagnostics_enabled,
                });
            }
        }
        state = shared
            .available
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

fn finish_work(
    shared: &Shared,
    work: &PreparationWork,
    result: std::result::Result<(), String>,
    elapsed: Duration,
) -> LevelWarmerEvent {
    let index_diagnostics = work
        .index_diagnostics
        .as_ref()
        .map_or_else(Vec::new, |events| {
            std::mem::take(
                &mut *events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        });
    let mut state = lock_state(shared);
    let was_cancelled = work.control.cancellation().is_cancelled();
    if state.active.as_ref().is_some_and(|active| {
        active.plan_id == work.plan_id
            && active.study_generation == work.study_generation
            && active.level == work.level
    }) {
        state.active = None;
    }

    if was_cancelled {
        if state.metrics_generation == Some(work.study_generation) {
            state.cancelled_total = state.cancelled_total.saturating_add(1);
        }
        return LevelWarmerEvent::Cancelled {
            study_generation: work.study_generation,
            level: work.level,
            elapsed,
            index_diagnostics,
        };
    }

    let plan = state
        .plan
        .as_mut()
        .filter(|plan| plan.id == work.plan_id && plan.study_generation == work.study_generation);
    match result {
        Ok(()) => {
            if let Some(plan) = plan {
                plan.prepared.insert(work.level);
            }
            if state.metrics_generation == Some(work.study_generation) {
                state.prepared_total = state.prepared_total.saturating_add(1);
            }
            LevelWarmerEvent::Prepared {
                study_generation: work.study_generation,
                level: work.level,
                elapsed,
                index_diagnostics,
            }
        }
        Err(error) => {
            if let Some(plan) = plan {
                plan.failed.insert(work.level);
            }
            if state.metrics_generation == Some(work.study_generation) {
                state.failed_total = state.failed_total.saturating_add(1);
            }
            LevelWarmerEvent::Failed {
                study_generation: work.study_generation,
                level: work.level,
                error,
                elapsed,
                index_diagnostics,
            }
        }
    }
}

fn emit_event(shared: &Shared, sender: &SyncSender<LevelWarmerEvent>, event: LevelWarmerEvent) {
    if let Err(mpsc::TrySendError::Full(event)) = sender.try_send(event) {
        let generation = event.study_generation();
        let mut state = lock_state(shared);
        if state.metrics_generation == Some(generation) {
            state.events_dropped = state.events_dropped.saturating_add(1);
        }
    }
}

fn cancel_active(state: &WarmerState) {
    if let Some(active) = &state.active {
        active.token.cancel();
    }
}

fn deduplicate_levels(priorities: Vec<LevelIndex>) -> Vec<LevelIndex> {
    let mut seen = HashSet::with_capacity(priorities.len());
    priorities
        .into_iter()
        .filter(|level| seen.insert(*level))
        .collect()
}

fn lock_state(shared: &Shared) -> MutexGuard<'_, WarmerState> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn panic_message(panic: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use dicom_viewer_core::{
        DicomIndexDiagnostic, DicomIndexMapping, DicomIndexOutcome, LevelIndex,
        ReadCancellationToken, ReadControl,
    };

    use super::{LevelPreparer, LevelWarmer, LevelWarmerEvent};

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);
    type PreparationResult = std::result::Result<(), String>;
    type GatedPreparerChannels = (
        Arc<GatePreparer>,
        Receiver<(LevelIndex, ReadCancellationToken)>,
        SyncSender<PreparationResult>,
    );

    struct GatePreparer {
        started: SyncSender<(LevelIndex, ReadCancellationToken)>,
        releases: Mutex<Receiver<PreparationResult>>,
    }

    impl LevelPreparer for GatePreparer {
        fn prepare_level_controlled(
            &self,
            level: LevelIndex,
            control: &ReadControl,
        ) -> std::result::Result<(), String> {
            self.started
                .send((level, control.cancellation().clone()))
                .expect("test should still be observing preparation starts");
            self.releases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv_timeout(TEST_TIMEOUT)
                .map_err(|error| format!("test did not release preparation: {error}"))?
        }
    }

    fn gated_preparer() -> GatedPreparerChannels {
        let (started_sender, started_receiver) = mpsc::sync_channel(8);
        let (release_sender, release_receiver) = mpsc::sync_channel(0);
        (
            Arc::new(GatePreparer {
                started: started_sender,
                releases: Mutex::new(release_receiver),
            }),
            started_receiver,
            release_sender,
        )
    }

    struct ImmediatePreparer {
        results: Mutex<VecDeque<std::result::Result<(), String>>>,
        prepared: SyncSender<LevelIndex>,
    }

    struct DiagnosticPreparer;

    impl LevelPreparer for DiagnosticPreparer {
        fn prepare_level_controlled(
            &self,
            _level: LevelIndex,
            control: &ReadControl,
        ) -> std::result::Result<(), String> {
            control.record_diagnostic(DicomIndexDiagnostic::new(
                DicomIndexOutcome::BuiltFast {
                    mapping: DicomIndexMapping::ExtendedOffsetTableDirect,
                },
                Duration::from_millis(7),
            ));
            Ok(())
        }
    }

    impl LevelPreparer for ImmediatePreparer {
        fn prepare_level_controlled(
            &self,
            level: LevelIndex,
            _control: &ReadControl,
        ) -> std::result::Result<(), String> {
            self.prepared
                .send(level)
                .expect("test should still be observing preparations");
            self.results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .unwrap_or(Ok(()))
        }
    }

    fn level(index: u32) -> LevelIndex {
        LevelIndex::from_u32(index)
    }

    fn recv_event(warmer: &LevelWarmer) -> LevelWarmerEvent {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            match warmer.try_recv() {
                Ok(event) => return event,
                Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                Err(error) => panic!("expected warmer event: {error}"),
            }
        }
    }

    #[test]
    fn prepares_each_level_once_in_published_order() {
        let (prepared_sender, prepared_receiver) = mpsc::sync_channel(8);
        let preparer = Arc::new(ImmediatePreparer {
            results: Mutex::new(VecDeque::new()),
            prepared: prepared_sender,
        });
        let warmer = LevelWarmer::with_event_capacity(8).expect("worker should start");

        warmer.publish_preparer(
            preparer.clone(),
            7,
            vec![level(2), level(0), level(1), level(2)],
        );

        assert_eq!(
            (0..3)
                .map(|_| {
                    prepared_receiver
                        .recv_timeout(TEST_TIMEOUT)
                        .expect("level should be prepared")
                })
                .collect::<Vec<_>>(),
            vec![level(2), level(0), level(1)]
        );
        for expected in [level(2), level(0), level(1)] {
            assert!(matches!(
                recv_event(&warmer),
                LevelWarmerEvent::Prepared {
                    study_generation: 7,
                    level,
                    ..
                } if level == expected
            ));
        }

        warmer.publish_preparer(preparer, 7, vec![level(1), level(0), level(2)]);
        assert!(matches!(warmer.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let stats = warmer.stats();
        assert_eq!(stats.prepared_levels, 3);
        assert_eq!(stats.pending_levels, 0);
        assert_eq!(stats.prepared_total, 3);
    }

    #[test]
    fn reprioritizes_pending_levels_without_cancelling_active_work() {
        let (preparer, started, release) = gated_preparer();
        let warmer = LevelWarmer::with_event_capacity(8).expect("worker should start");
        warmer.publish_preparer(preparer.clone(), 11, vec![level(0), level(1), level(2)]);
        let (active, active_token) = started
            .recv_timeout(TEST_TIMEOUT)
            .expect("first level should start");
        assert_eq!(active, level(0));

        warmer.publish_preparer(preparer, 11, vec![level(2), level(1)]);
        assert!(!active_token.is_cancelled());
        release
            .send(Ok(()))
            .expect("active level should be released");
        assert!(matches!(
            recv_event(&warmer),
            LevelWarmerEvent::Prepared { level: done, .. } if done == level(0)
        ));

        let (next, _) = started
            .recv_timeout(TEST_TIMEOUT)
            .expect("reprioritized level should start");
        assert_eq!(next, level(2));
        release
            .send(Ok(()))
            .expect("second level should be released");
        let _ = recv_event(&warmer);
        let (last, _) = started
            .recv_timeout(TEST_TIMEOUT)
            .expect("remaining level should start");
        assert_eq!(last, level(1));
        release.send(Ok(())).expect("last level should be released");
        let _ = recv_event(&warmer);
    }

    #[test]
    fn replacing_a_study_cancels_active_and_discards_old_pending_work() {
        let (old_preparer, old_started, old_release) = gated_preparer();
        let (new_preparer, new_started, new_release) = gated_preparer();
        let warmer = LevelWarmer::with_event_capacity(8).expect("worker should start");
        warmer.publish_preparer(old_preparer, 1, vec![level(0), level(1)]);
        let (_, old_token) = old_started
            .recv_timeout(TEST_TIMEOUT)
            .expect("old level should start");

        warmer.publish_preparer(new_preparer, 2, vec![level(4)]);
        assert!(old_token.is_cancelled());
        old_release
            .send(Ok(()))
            .expect("cancelled preparation should be released");
        assert!(matches!(
            recv_event(&warmer),
            LevelWarmerEvent::Cancelled {
                study_generation: 1,
                level: cancelled,
                ..
            } if cancelled == level(0)
        ));

        let (new_level, _) = new_started
            .recv_timeout(TEST_TIMEOUT)
            .expect("new study should start next");
        assert_eq!(new_level, level(4));
        new_release
            .send(Ok(()))
            .expect("new preparation should be released");
        assert!(matches!(
            recv_event(&warmer),
            LevelWarmerEvent::Prepared {
                study_generation: 2,
                level: prepared,
                ..
            } if prepared == level(4)
        ));
        let stats = warmer.stats();
        assert_eq!(stats.cancelled_total, 0);
        assert_eq!(stats.prepared_total, 1);
    }

    #[test]
    fn clear_cancels_active_and_removes_pending_levels() {
        let (preparer, started, release) = gated_preparer();
        let warmer = LevelWarmer::with_event_capacity(8).expect("worker should start");
        warmer.publish_preparer(preparer, 5, vec![level(0), level(1)]);
        let (_, token) = started
            .recv_timeout(TEST_TIMEOUT)
            .expect("level should start");

        warmer.clear();
        assert!(token.is_cancelled());
        release
            .send(Ok(()))
            .expect("cancelled preparation should be released");
        assert!(matches!(
            recv_event(&warmer),
            LevelWarmerEvent::Cancelled {
                study_generation: 5,
                ..
            }
        ));
        let stats = warmer.stats();
        assert_eq!(stats.study_generation, None);
        assert_eq!(stats.pending_levels, 0);
        assert_eq!(stats.active_level, None);
    }

    #[test]
    fn failed_levels_are_reported_and_not_retried_on_republish() {
        let (prepared_sender, prepared_receiver) = mpsc::sync_channel(8);
        let preparer = Arc::new(ImmediatePreparer {
            results: Mutex::new(VecDeque::from([Err("invalid frame index".into())])),
            prepared: prepared_sender,
        });
        let warmer = LevelWarmer::with_event_capacity(8).expect("worker should start");
        warmer.publish_preparer(preparer.clone(), 9, vec![level(3)]);
        assert_eq!(
            prepared_receiver
                .recv_timeout(TEST_TIMEOUT)
                .expect("failed level should be attempted"),
            level(3)
        );
        assert!(matches!(
            recv_event(&warmer),
            LevelWarmerEvent::Failed {
                study_generation: 9,
                level: failed,
                ref error,
                ..
            } if failed == level(3) && error == "invalid frame index"
        ));

        warmer.publish_preparer(preparer, 9, vec![level(3)]);
        assert!(matches!(warmer.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let stats = warmer.stats();
        assert_eq!(stats.failed_levels, 1);
        assert_eq!(stats.failed_total, 1);
    }

    #[test]
    fn bounded_event_channel_drops_diagnostics_without_blocking_worker() {
        let (prepared_sender, prepared_receiver) = mpsc::sync_channel(8);
        let preparer = Arc::new(ImmediatePreparer {
            results: Mutex::new(VecDeque::new()),
            prepared: prepared_sender,
        });
        let warmer = LevelWarmer::with_event_capacity(1).expect("worker should start");
        warmer.publish_preparer(preparer, 3, vec![level(0), level(1), level(2)]);

        for expected in [level(0), level(1), level(2)] {
            assert_eq!(
                prepared_receiver
                    .recv_timeout(TEST_TIMEOUT)
                    .expect("level should be attempted"),
                expected
            );
        }
        let deadline = Instant::now() + TEST_TIMEOUT;
        while warmer.stats().prepared_total < 3 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let stats = warmer.stats();
        assert_eq!(stats.prepared_total, 3);
        assert_eq!(stats.events_dropped, 2);
        assert!(matches!(
            warmer.try_recv(),
            Ok(LevelWarmerEvent::Prepared { .. })
        ));
    }

    #[test]
    fn drop_cancels_active_preparation_before_joining_worker() {
        let (preparer, started, release) = gated_preparer();
        let warmer = LevelWarmer::with_event_capacity(8).expect("worker should start");
        warmer.publish_preparer(preparer, 21, vec![level(0)]);
        let (_, token) = started
            .recv_timeout(TEST_TIMEOUT)
            .expect("level should start");

        let dropper = std::thread::spawn(move || drop(warmer));
        let deadline = Instant::now() + TEST_TIMEOUT;
        while !token.is_cancelled() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(token.is_cancelled());
        release
            .send(Ok(()))
            .expect("cancelled preparation should be released");
        dropper.join().expect("warmer drop should finish");
    }

    #[test]
    fn diagnostics_are_collected_only_for_opted_in_preparation() {
        let warmer = LevelWarmer::with_event_capacity(8).expect("worker should start");
        warmer.publish_preparer_with_diagnostics(
            Arc::new(DiagnosticPreparer),
            31,
            vec![level(0)],
            true,
        );

        let LevelWarmerEvent::Prepared {
            index_diagnostics, ..
        } = recv_event(&warmer)
        else {
            panic!("diagnostic preparation should succeed");
        };
        assert_eq!(
            index_diagnostics,
            vec![DicomIndexDiagnostic::new(
                DicomIndexOutcome::BuiltFast {
                    mapping: DicomIndexMapping::ExtendedOffsetTableDirect,
                },
                Duration::from_millis(7),
            )]
        );

        warmer.publish_preparer_with_diagnostics(
            Arc::new(DiagnosticPreparer),
            32,
            vec![level(0)],
            false,
        );
        let LevelWarmerEvent::Prepared {
            index_diagnostics,
            elapsed,
            ..
        } = recv_event(&warmer)
        else {
            panic!("non-diagnostic preparation should succeed");
        };
        assert!(index_diagnostics.is_empty());
        assert_eq!(elapsed, Duration::ZERO);
    }

    #[test]
    fn disabled_preparation_diagnostics_do_not_sample_the_clock() {
        let clock_calls = std::cell::Cell::new(0);

        let started = super::diagnostic_timer_with(false, || {
            clock_calls.set(clock_calls.get() + 1);
            Instant::now()
        });

        assert!(started.is_none());
        assert_eq!(clock_calls.get(), 0);
    }
}
