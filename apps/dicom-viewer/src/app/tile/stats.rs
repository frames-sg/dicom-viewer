use std::collections::VecDeque;
use std::time::{Duration, Instant};

use dicom_viewer_core::{DicomIndexDiagnostic, DicomIndexMapping, DicomIndexOutcome, LevelIndex};

use super::{
    loader::{TileBatchMetrics, TileLoaderStats},
    QueueLane,
};

mod json;

use json::{interaction_json, pipeline_json};

const EMIT_INTERVAL: Duration = Duration::from_secs(1);
pub(super) const PIPELINE_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum LevelPreparationStatus {
    Prepared,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum DicomIndexDiagnosticSource {
    Preparation,
    Read,
}

pub(super) struct PipelineStats {
    enabled: Option<EnabledPipelineStats>,
}

struct EnabledPipelineStats {
    started: Instant,
    last_emitted: Instant,
    window: RollingWindow,
    lifetime: LifetimeCounters,
    gauges: Gauges,
    interaction: Option<Interaction>,
}

#[derive(Default)]
struct RollingWindow {
    queue_wait_ms: VecDeque<TimedSample>,
    visible_queue_wait_ms: VecDeque<TimedSample>,
    source_read_ms: VecDeque<TimedSample>,
    upload_ms: VecDeque<TimedSample>,
    app_ui_cpu_ms: VecDeque<TimedSample>,
    eviction_ms: VecDeque<TimedSample>,
    level_preparation_ms: VecDeque<TimedSample>,
    dicom_index: RollingDicomIndexSamples,
    batches: VecDeque<TimedBatch>,
}

#[derive(Default)]
struct RollingDicomIndexSamples {
    built_fast_ms: VecDeque<TimedSample>,
    fast_path_fallback_ms: VecDeque<TimedSample>,
    token_fallback_ms: VecDeque<TimedSample>,
    reused_ms: VecDeque<TimedSample>,
}

impl RollingDicomIndexSamples {
    fn record(&mut self, outcome: DicomIndexOutcome, sample: TimedSample) {
        match outcome {
            DicomIndexOutcome::BuiltFast { .. } => self.built_fast_ms.push_back(sample),
            DicomIndexOutcome::FastPathFallback => self.fast_path_fallback_ms.push_back(sample),
            DicomIndexOutcome::TokenFallback => self.token_fallback_ms.push_back(sample),
            DicomIndexOutcome::Reused => self.reused_ms.push_back(sample),
            _ => {}
        }
    }

    fn snapshot(&self, now: Instant) -> DicomIndexSamples {
        DicomIndexSamples {
            built_fast_ms: trailing_values(&self.built_fast_ms, now),
            fast_path_fallback_ms: trailing_values(&self.fast_path_fallback_ms, now),
            token_fallback_ms: trailing_values(&self.token_fallback_ms, now),
            reused_ms: trailing_values(&self.reused_ms, now),
        }
    }

    fn evict_expired(&mut self, now: Instant) {
        evict_expired_samples(&mut self.built_fast_ms, now);
        evict_expired_samples(&mut self.fast_path_fallback_ms, now);
        evict_expired_samples(&mut self.token_fallback_ms, now);
        evict_expired_samples(&mut self.reused_ms, now);
    }
}

#[derive(Clone, Copy)]
struct TimedSample {
    at: Instant,
    value: f64,
}

#[derive(Clone, Copy)]
struct TimedBatch {
    at: Instant,
    requested_tiles: u64,
    admitted_tiles: u64,
    returned_tiles: u64,
    obsolete_source_read_ms: f64,
}

impl RollingWindow {
    fn snapshot(&self, now: Instant) -> Window {
        let mut snapshot = Window {
            queue_wait_ms: trailing_values(&self.queue_wait_ms, now),
            visible_queue_wait_ms: trailing_values(&self.visible_queue_wait_ms, now),
            source_read_ms: trailing_values(&self.source_read_ms, now),
            upload_ms: trailing_values(&self.upload_ms, now),
            app_ui_cpu_ms: trailing_values(&self.app_ui_cpu_ms, now),
            eviction_ms: trailing_values(&self.eviction_ms, now),
            level_preparation_ms: trailing_values(&self.level_preparation_ms, now),
            dicom_index: self.dicom_index.snapshot(now),
            ..Window::default()
        };
        for batch in self
            .batches
            .iter()
            .filter(|batch| is_in_trailing_window(batch.at, now))
        {
            snapshot.requested_tiles = snapshot
                .requested_tiles
                .saturating_add(batch.requested_tiles);
            snapshot.admitted_tiles = snapshot.admitted_tiles.saturating_add(batch.admitted_tiles);
            snapshot.returned_tiles = snapshot.returned_tiles.saturating_add(batch.returned_tiles);
            snapshot.obsolete_source_read_ms += batch.obsolete_source_read_ms;
        }
        snapshot
    }

    fn evict_expired(&mut self, now: Instant) {
        evict_expired_samples(&mut self.queue_wait_ms, now);
        evict_expired_samples(&mut self.visible_queue_wait_ms, now);
        evict_expired_samples(&mut self.source_read_ms, now);
        evict_expired_samples(&mut self.upload_ms, now);
        evict_expired_samples(&mut self.app_ui_cpu_ms, now);
        evict_expired_samples(&mut self.eviction_ms, now);
        evict_expired_samples(&mut self.level_preparation_ms, now);
        self.dicom_index.evict_expired(now);
        while self
            .batches
            .front()
            .is_some_and(|batch| !is_in_trailing_window(batch.at, now))
        {
            self.batches.pop_front();
        }
    }
}

fn trailing_values(samples: &VecDeque<TimedSample>, now: Instant) -> Vec<f64> {
    samples
        .iter()
        .filter(|sample| is_in_trailing_window(sample.at, now))
        .map(|sample| sample.value)
        .collect()
}

fn evict_expired_samples(samples: &mut VecDeque<TimedSample>, now: Instant) {
    while samples
        .front()
        .is_some_and(|sample| !is_in_trailing_window(sample.at, now))
    {
        samples.pop_front();
    }
}

fn is_in_trailing_window(at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(at) <= EMIT_INTERVAL
}

#[derive(Default)]
struct Window {
    queue_wait_ms: Vec<f64>,
    visible_queue_wait_ms: Vec<f64>,
    source_read_ms: Vec<f64>,
    upload_ms: Vec<f64>,
    app_ui_cpu_ms: Vec<f64>,
    eviction_ms: Vec<f64>,
    level_preparation_ms: Vec<f64>,
    dicom_index: DicomIndexSamples,
    obsolete_source_read_ms: f64,
    requested_tiles: u64,
    admitted_tiles: u64,
    returned_tiles: u64,
}

#[derive(Default)]
struct DicomIndexSamples {
    built_fast_ms: Vec<f64>,
    fast_path_fallback_ms: Vec<f64>,
    token_fallback_ms: Vec<f64>,
    reused_ms: Vec<f64>,
}

impl DicomIndexSamples {
    fn record(&mut self, outcome: DicomIndexOutcome, milliseconds: f64) {
        match outcome {
            DicomIndexOutcome::BuiltFast { .. } => self.built_fast_ms.push(milliseconds),
            DicomIndexOutcome::FastPathFallback => {
                self.fast_path_fallback_ms.push(milliseconds);
            }
            DicomIndexOutcome::TokenFallback => self.token_fallback_ms.push(milliseconds),
            DicomIndexOutcome::Reused => self.reused_ms.push(milliseconds),
            _ => {}
        }
    }
}

#[derive(Default, Clone, Copy)]
struct LifetimeCounters {
    requested_tiles: u64,
    admitted_tiles: u64,
    ready_cache_hits: u64,
    in_flight_deduplications: u64,
    enqueued: u64,
    returned_tiles: u64,
    cpu_results: u64,
    metal_results: u64,
    retries: u64,
    failures: u64,
    demand_cancellations: u64,
    source_cancellations: u64,
    preparation_cancellations: u64,
    level_preparations: u64,
    level_preparation_failures: u64,
    obsolete_discards: u64,
    uploads: u64,
    evictions: u64,
    dicom_index: DicomIndexCounters,
}

#[derive(Default, Clone, Copy)]
struct DicomIndexCounters {
    preparation_events: u64,
    read_events: u64,
    built_fast: u64,
    fast_path_fallback: u64,
    token_fallback: u64,
    reused: u64,
    extended_offset_table_direct: u64,
    extended_offset_table_items: u64,
    basic_offset_table_items: u64,
    single_frame_items: u64,
    one_fragment_per_frame: u64,
}

impl DicomIndexCounters {
    fn record(&mut self, source: DicomIndexDiagnosticSource, outcome: DicomIndexOutcome) {
        match source {
            DicomIndexDiagnosticSource::Preparation => {
                self.preparation_events = self.preparation_events.saturating_add(1);
            }
            DicomIndexDiagnosticSource::Read => {
                self.read_events = self.read_events.saturating_add(1);
            }
        }
        match outcome {
            DicomIndexOutcome::BuiltFast { mapping } => {
                self.built_fast = self.built_fast.saturating_add(1);
                match mapping {
                    DicomIndexMapping::ExtendedOffsetTableDirect => {
                        self.extended_offset_table_direct =
                            self.extended_offset_table_direct.saturating_add(1);
                    }
                    DicomIndexMapping::ExtendedOffsetTableItems => {
                        self.extended_offset_table_items =
                            self.extended_offset_table_items.saturating_add(1);
                    }
                    DicomIndexMapping::BasicOffsetTableItems => {
                        self.basic_offset_table_items =
                            self.basic_offset_table_items.saturating_add(1);
                    }
                    DicomIndexMapping::SingleFrameItems => {
                        self.single_frame_items = self.single_frame_items.saturating_add(1);
                    }
                    DicomIndexMapping::OneFragmentPerFrame => {
                        self.one_fragment_per_frame = self.one_fragment_per_frame.saturating_add(1);
                    }
                    _ => {}
                }
            }
            DicomIndexOutcome::FastPathFallback => {
                self.fast_path_fallback = self.fast_path_fallback.saturating_add(1);
            }
            DicomIndexOutcome::TokenFallback => {
                self.token_fallback = self.token_fallback.saturating_add(1);
            }
            DicomIndexOutcome::Reused => self.reused = self.reused.saturating_add(1),
            _ => {}
        }
    }

    fn delta(self, start: Self) -> Self {
        Self {
            preparation_events: self
                .preparation_events
                .saturating_sub(start.preparation_events),
            read_events: self.read_events.saturating_sub(start.read_events),
            built_fast: self.built_fast.saturating_sub(start.built_fast),
            fast_path_fallback: self
                .fast_path_fallback
                .saturating_sub(start.fast_path_fallback),
            token_fallback: self.token_fallback.saturating_sub(start.token_fallback),
            reused: self.reused.saturating_sub(start.reused),
            extended_offset_table_direct: self
                .extended_offset_table_direct
                .saturating_sub(start.extended_offset_table_direct),
            extended_offset_table_items: self
                .extended_offset_table_items
                .saturating_sub(start.extended_offset_table_items),
            basic_offset_table_items: self
                .basic_offset_table_items
                .saturating_sub(start.basic_offset_table_items),
            single_frame_items: self
                .single_frame_items
                .saturating_sub(start.single_frame_items),
            one_fragment_per_frame: self
                .one_fragment_per_frame
                .saturating_sub(start.one_fragment_per_frame),
        }
    }
}

#[derive(Default)]
struct Gauges {
    planned: usize,
    queued: usize,
    visible: usize,
    transition: usize,
    fallback: usize,
    overview: usize,
    prefetch: usize,
    decoding: usize,
    resident_bytes: usize,
    pinned_bytes: usize,
    submissions: u64,
}

struct Interaction {
    started: Instant,
    last_zoom_input: Instant,
    target_level: Option<LevelIndex>,
    first_sharp_target_ms: Option<f64>,
    full_target_coverage_ms: Option<f64>,
    camera_settled: bool,
    target_coverage: TargetCoverage,
    samples: Window,
    counters_at_start: LifetimeCounters,
}

#[derive(Clone, Copy, Default)]
struct TargetCoverage {
    ready: usize,
    pending: usize,
    failed: usize,
    missing: usize,
}

#[derive(Clone, Copy)]
struct Distribution {
    count: usize,
    p50: f64,
    p95: f64,
    p99: f64,
}

impl PipelineStats {
    pub(super) fn from_environment() -> Self {
        if std::env::var("DICOM_VIEWER_DEBUG_STATS").as_deref() == Ok("1") {
            Self::enabled_at(Instant::now())
        } else {
            Self::disabled()
        }
    }

    fn enabled_at(now: Instant) -> Self {
        Self {
            enabled: Some(EnabledPipelineStats {
                started: now,
                last_emitted: now,
                window: RollingWindow::default(),
                lifetime: LifetimeCounters::default(),
                gauges: Gauges::default(),
                interaction: None,
            }),
        }
    }

    const fn disabled() -> Self {
        Self { enabled: None }
    }

    pub(super) const fn is_enabled(&self) -> bool {
        self.enabled.is_some()
    }

    pub(super) fn request_periodic_repaint(&self, request: impl FnOnce(Duration)) {
        if let Some(delay) = self.next_emit_delay_at(Instant::now()) {
            request(delay);
        }
    }

    fn next_emit_delay_at(&self, now: Instant) -> Option<Duration> {
        let stats = self.enabled.as_ref()?;
        Some(EMIT_INTERVAL.saturating_sub(now.saturating_duration_since(stats.last_emitted)))
    }

    pub(super) fn clear_study_counters(&mut self) {
        let Some(stats) = &mut self.enabled else {
            return;
        };
        stats.window = RollingWindow::default();
        stats.gauges = Gauges::default();
        stats.interaction = None;
        stats.last_emitted = Instant::now();
    }

    pub(super) fn set_interactive(&mut self, interactive: bool) {
        if self.enabled.is_none() {
            return;
        }
        self.set_interactive_at(interactive, Instant::now());
    }

    fn set_interactive_at(&mut self, interactive: bool, now: Instant) -> Option<String> {
        let Some(stats) = &mut self.enabled else {
            return None;
        };
        match (interactive, stats.interaction.is_some()) {
            (true, false) => {
                stats.interaction = Some(Interaction {
                    started: now,
                    last_zoom_input: now,
                    target_level: None,
                    first_sharp_target_ms: None,
                    full_target_coverage_ms: None,
                    camera_settled: false,
                    target_coverage: TargetCoverage::default(),
                    samples: Window::default(),
                    counters_at_start: stats.lifetime,
                });
                None
            }
            (true, true) => {
                if let Some(interaction) = &mut stats.interaction {
                    interaction.camera_settled = false;
                }
                None
            }
            (false, true) => {
                if let Some(interaction) = &mut stats.interaction {
                    interaction.camera_settled = true;
                }
                None
            }
            (false, false) => None,
        }
    }

    pub(super) fn record_zoom_input(&mut self) {
        if self.enabled.is_none() {
            return;
        }
        self.record_zoom_input_at(Instant::now());
    }

    fn record_zoom_input_at(&mut self, now: Instant) {
        let Some(stats) = &mut self.enabled else {
            return;
        };
        if stats.interaction.is_none() {
            stats.interaction = Some(Interaction {
                started: now,
                last_zoom_input: now,
                target_level: None,
                first_sharp_target_ms: None,
                full_target_coverage_ms: None,
                camera_settled: false,
                target_coverage: TargetCoverage::default(),
                samples: Window::default(),
                counters_at_start: stats.lifetime,
            });
            return;
        }
        let interaction = stats
            .interaction
            .as_mut()
            .expect("interaction was initialized above");
        interaction.last_zoom_input = now;
        interaction.full_target_coverage_ms = None;
        interaction.camera_settled = false;
        interaction.target_coverage = TargetCoverage::default();
    }

    pub(super) fn observe_target(
        &mut self,
        level: LevelIndex,
        ready: usize,
        pending: usize,
        failed: usize,
        missing: usize,
    ) {
        if self.enabled.is_none() {
            return;
        }
        if let Some(line) =
            self.observe_target_at(level, ready, pending, failed, missing, Instant::now())
        {
            eprintln!("{line}");
        }
    }

    fn observe_target_at(
        &mut self,
        level: LevelIndex,
        ready: usize,
        pending: usize,
        failed: usize,
        missing: usize,
        now: Instant,
    ) -> Option<String> {
        let stats = self.enabled.as_mut()?;
        let interaction = stats.interaction.as_mut()?;
        if interaction.target_level != Some(level) {
            interaction.target_level = Some(level);
            interaction.first_sharp_target_ms = None;
            interaction.full_target_coverage_ms = None;
        }
        interaction.target_coverage = TargetCoverage {
            ready,
            pending,
            failed,
            missing,
        };
        let since_gesture_start_ms = now
            .saturating_duration_since(interaction.started)
            .as_secs_f64()
            * 1_000.0;
        let since_last_input_ms = now
            .saturating_duration_since(interaction.last_zoom_input)
            .as_secs_f64()
            * 1_000.0;
        if ready > 0 && interaction.first_sharp_target_ms.is_none() {
            interaction.first_sharp_target_ms = Some(since_gesture_start_ms);
        }
        let total = ready
            .saturating_add(pending)
            .saturating_add(failed)
            .saturating_add(missing);
        let fully_covered = total > 0 && ready == total;
        if fully_covered && interaction.full_target_coverage_ms.is_none() {
            interaction.full_target_coverage_ms = Some(since_last_input_ms);
        }
        let final_coverage_known = total > 0 && pending == 0 && missing == 0;
        if !interaction.camera_settled || !final_coverage_known {
            return None;
        }
        let interaction = stats
            .interaction
            .take()
            .expect("interaction was present while observing coverage");
        Some(interaction_json(stats, interaction, now))
    }

    pub(super) fn record_app_ui_cpu_time(&mut self, elapsed: Duration) {
        if self.enabled.is_none() {
            return;
        }
        self.record_app_ui_cpu_time_at(elapsed, Instant::now());
    }

    fn record_app_ui_cpu_time_at(&mut self, elapsed: Duration, now: Instant) {
        let Some(stats) = &mut self.enabled else {
            return;
        };
        let milliseconds = elapsed.as_secs_f64() * 1_000.0;
        stats.window.app_ui_cpu_ms.push_back(TimedSample {
            at: now,
            value: milliseconds,
        });
        if let Some(interaction) = &mut stats.interaction {
            interaction.samples.app_ui_cpu_ms.push(milliseconds);
        }
    }

    pub(super) fn record_eviction(&mut self, elapsed: Duration) {
        if self.enabled.is_none() {
            return;
        }
        self.record_eviction_at(elapsed, Instant::now());
    }

    fn record_eviction_at(&mut self, elapsed: Duration, now: Instant) {
        let Some(stats) = &mut self.enabled else {
            return;
        };
        let milliseconds = elapsed.as_secs_f64() * 1_000.0;
        stats.window.eviction_ms.push_back(TimedSample {
            at: now,
            value: milliseconds,
        });
        stats.lifetime.evictions = stats.lifetime.evictions.saturating_add(1);
        if let Some(interaction) = &mut stats.interaction {
            interaction.samples.eviction_ms.push(milliseconds);
        }
    }

    pub(super) fn record_batch_with_obsolete(
        &mut self,
        metrics: TileBatchMetrics,
        obsolete_results: usize,
    ) {
        if self.enabled.is_none() {
            return;
        }
        self.record_batch_with_obsolete_at(metrics, obsolete_results, Instant::now());
    }

    fn record_batch_with_obsolete_at(
        &mut self,
        metrics: TileBatchMetrics,
        obsolete_results: usize,
        now: Instant,
    ) {
        let Some(stats) = &mut self.enabled else {
            return;
        };
        for queue_wait in &metrics.queue_waits {
            stats.window.queue_wait_ms.push_back(TimedSample {
                at: now,
                value: queue_wait.milliseconds,
            });
            if queue_wait.lane == QueueLane::Visible {
                stats.window.visible_queue_wait_ms.push_back(TimedSample {
                    at: now,
                    value: queue_wait.milliseconds,
                });
            }
        }
        let obsolete_fraction =
            obsolete_results.min(metrics.requested) as f64 / metrics.requested.max(1) as f64;
        let mut obsolete_source_read_ms = 0.0;
        if let Some(source_read) = metrics.source_read_ms {
            stats.window.source_read_ms.push_back(TimedSample {
                at: now,
                value: source_read,
            });
            // One source-read duration belongs to the whole batch. Attribute
            // only the obsolete result fraction so a partially useful batch
            // is not counted once per discarded tile (or counted twice).
            obsolete_source_read_ms = source_read * obsolete_fraction;
        }
        let requested = metrics.requested as u64;
        let admitted = metrics.admitted as u64;
        let returned = metrics.returned as u64;
        stats.window.batches.push_back(TimedBatch {
            at: now,
            requested_tiles: requested,
            admitted_tiles: admitted,
            returned_tiles: returned,
            obsolete_source_read_ms,
        });
        stats.lifetime.requested_tiles = stats.lifetime.requested_tiles.saturating_add(requested);
        stats.lifetime.admitted_tiles = stats.lifetime.admitted_tiles.saturating_add(admitted);
        stats.lifetime.returned_tiles = stats.lifetime.returned_tiles.saturating_add(returned);
        stats.lifetime.cpu_results = stats
            .lifetime
            .cpu_results
            .saturating_add(metrics.cpu_results as u64);
        stats.lifetime.metal_results = stats
            .lifetime
            .metal_results
            .saturating_add(metrics.metal_results as u64);
        stats.lifetime.retries = stats
            .lifetime
            .retries
            .saturating_add(metrics.retries as u64);
        stats.lifetime.failures = stats
            .lifetime
            .failures
            .saturating_add(metrics.failures as u64);
        stats.lifetime.source_cancellations = stats
            .lifetime
            .source_cancellations
            .saturating_add(metrics.source_cancellations as u64);
        if let Some(interaction) = &mut stats.interaction {
            for queue_wait in &metrics.queue_waits {
                interaction
                    .samples
                    .queue_wait_ms
                    .push(queue_wait.milliseconds);
                if queue_wait.lane == QueueLane::Visible {
                    interaction
                        .samples
                        .visible_queue_wait_ms
                        .push(queue_wait.milliseconds);
                }
            }
            if let Some(source_read) = metrics.source_read_ms {
                interaction.samples.source_read_ms.push(source_read);
                interaction.samples.obsolete_source_read_ms += source_read * obsolete_fraction;
            }
            interaction.samples.requested_tiles = interaction
                .samples
                .requested_tiles
                .saturating_add(requested);
            interaction.samples.admitted_tiles =
                interaction.samples.admitted_tiles.saturating_add(admitted);
            interaction.samples.returned_tiles =
                interaction.samples.returned_tiles.saturating_add(returned);
        }
    }

    pub(super) fn record_upload(&mut self, elapsed: Duration, planned: usize, uploaded: usize) {
        if self.enabled.is_none() || planned == 0 {
            return;
        }
        self.record_upload_at(elapsed, planned, uploaded, Instant::now());
    }

    fn record_upload_at(
        &mut self,
        elapsed: Duration,
        planned: usize,
        uploaded: usize,
        now: Instant,
    ) {
        if planned == 0 {
            return;
        }
        let Some(stats) = &mut self.enabled else {
            return;
        };
        let milliseconds = elapsed.as_secs_f64() * 1000.0;
        stats.window.upload_ms.push_back(TimedSample {
            at: now,
            value: milliseconds,
        });
        stats.lifetime.uploads = stats.lifetime.uploads.saturating_add(uploaded as u64);
        if let Some(interaction) = &mut stats.interaction {
            interaction.samples.upload_ms.push(milliseconds);
        }
    }

    pub(super) fn record_level_preparation(
        &mut self,
        elapsed: Duration,
        status: LevelPreparationStatus,
    ) {
        if self.enabled.is_none() {
            return;
        }
        self.record_level_preparation_at(elapsed, status, Instant::now());
    }

    fn record_level_preparation_at(
        &mut self,
        elapsed: Duration,
        status: LevelPreparationStatus,
        now: Instant,
    ) {
        let Some(stats) = &mut self.enabled else {
            return;
        };
        let milliseconds = elapsed.as_secs_f64() * 1_000.0;
        stats.window.level_preparation_ms.push_back(TimedSample {
            at: now,
            value: milliseconds,
        });
        if let Some(interaction) = &mut stats.interaction {
            interaction.samples.level_preparation_ms.push(milliseconds);
        }
        match status {
            LevelPreparationStatus::Prepared => {
                stats.lifetime.level_preparations =
                    stats.lifetime.level_preparations.saturating_add(1);
            }
            LevelPreparationStatus::Failed => {
                stats.lifetime.level_preparation_failures =
                    stats.lifetime.level_preparation_failures.saturating_add(1);
            }
            LevelPreparationStatus::Cancelled => {
                stats.lifetime.preparation_cancellations =
                    stats.lifetime.preparation_cancellations.saturating_add(1);
            }
        }
    }

    pub(super) fn record_dicom_index_diagnostics(
        &mut self,
        source: DicomIndexDiagnosticSource,
        diagnostics: &[DicomIndexDiagnostic],
    ) {
        if self.enabled.is_none() || diagnostics.is_empty() {
            return;
        }
        self.record_dicom_index_diagnostics_at(source, diagnostics, Instant::now());
    }

    fn record_dicom_index_diagnostics_at(
        &mut self,
        source: DicomIndexDiagnosticSource,
        diagnostics: &[DicomIndexDiagnostic],
        now: Instant,
    ) {
        let Some(stats) = &mut self.enabled else {
            return;
        };
        for diagnostic in diagnostics {
            let milliseconds = diagnostic.elapsed.as_secs_f64() * 1_000.0;
            stats.window.dicom_index.record(
                diagnostic.outcome,
                TimedSample {
                    at: now,
                    value: milliseconds,
                },
            );
            stats
                .lifetime
                .dicom_index
                .record(source, diagnostic.outcome);
            if let Some(interaction) = &mut stats.interaction {
                interaction
                    .samples
                    .dicom_index
                    .record(diagnostic.outcome, milliseconds);
            }
        }
    }

    pub(super) fn record_cache_hit(&mut self) {
        if let Some(stats) = &mut self.enabled {
            stats.lifetime.ready_cache_hits = stats.lifetime.ready_cache_hits.saturating_add(1);
        }
    }

    pub(super) fn record_deduplicated(&mut self, count: usize) {
        if let Some(stats) = &mut self.enabled {
            stats.lifetime.in_flight_deduplications = stats
                .lifetime
                .in_flight_deduplications
                .saturating_add(count as u64);
        }
    }

    pub(super) fn record_failures(&mut self, count: usize) {
        if let Some(stats) = &mut self.enabled {
            stats.lifetime.failures = stats.lifetime.failures.saturating_add(count as u64);
        }
    }

    pub(super) fn record_stale_work(&mut self) {
        if let Some(stats) = &mut self.enabled {
            stats.lifetime.obsolete_discards = stats.lifetime.obsolete_discards.saturating_add(1);
        }
    }

    pub(super) fn record_enqueued(&mut self, count: usize) {
        if let Some(stats) = &mut self.enabled {
            stats.lifetime.enqueued = stats.lifetime.enqueued.saturating_add(count as u64);
        }
    }

    pub(super) fn update_gauges(
        &mut self,
        planned: usize,
        loader: TileLoaderStats,
        resident_bytes: usize,
        pinned_bytes: usize,
        submissions: u64,
    ) {
        let Some(stats) = &mut self.enabled else {
            return;
        };
        stats.gauges.planned = planned;
        stats.gauges.queued = loader.queued;
        stats.gauges.visible = loader.visible;
        stats.gauges.transition = loader.transition;
        stats.gauges.fallback = loader.fallback;
        stats.gauges.overview = loader.overview;
        stats.gauges.prefetch = loader.prefetch;
        stats.gauges.decoding = loader.decoding;
        stats.gauges.resident_bytes = resident_bytes;
        stats.gauges.pinned_bytes = pinned_bytes;
        stats.gauges.submissions = submissions;
        stats.lifetime.demand_cancellations = loader.cancellations;
        if let Some(line) = self.pipeline_window_json(Instant::now()) {
            eprintln!("{line}");
        }
    }

    pub(super) fn overlay_text(&self) -> Option<String> {
        let stats = self.enabled.as_ref()?;
        let window = stats.window.snapshot(Instant::now());
        let wait = distribution(&window.queue_wait_ms);
        let visible_wait = distribution(&window.visible_queue_wait_ms);
        let read = distribution(&window.source_read_ms);
        let app_ui_cpu = distribution(&window.app_ui_cpu_ms);
        let eviction = distribution(&window.eviction_ms);
        let index_fast = distribution(&window.dicom_index.built_fast_ms);
        let index_token = distribution(&window.dicom_index.token_fallback_ms);
        let index_reused = distribution(&window.dicom_index.reused_ms);
        let source_read_total_ms = window.source_read_ms.iter().sum::<f64>();
        let obsolete_ratio = safe_ratio(window.obsolete_source_read_ms, source_read_total_ms);
        Some(format!(
            "pipeline queue v/t/f/o/p={}/{}/{}/{}/{} total/decoding={}/{} wait visible p50/p95={:.1}/{:.1}ms all={:.1}/{:.1}ms read={:.1}/{:.1}ms app-ui-cpu p50/p95/p99={:.1}/{:.1}/{:.1}ms eviction n/p95={}/{:.1}ms index fast/token/reuse={}/{}/{} p95={:.1}/{:.1}/{:.1}ms obsolete-read={:.1}ms/{:.1}% ready-hit={} dedup={} resident/pinned={:.1}/{:.1} MiB cancel={} obsolete={} submit={}",
            stats.gauges.visible,
            stats.gauges.transition,
            stats.gauges.fallback,
            stats.gauges.overview,
            stats.gauges.prefetch,
            stats.gauges.queued,
            stats.gauges.decoding,
            visible_wait.p50,
            visible_wait.p95,
            wait.p50,
            wait.p95,
            read.p50,
            read.p95,
            app_ui_cpu.p50,
            app_ui_cpu.p95,
            app_ui_cpu.p99,
            eviction.count,
            eviction.p95,
            stats.lifetime.dicom_index.built_fast,
            stats.lifetime.dicom_index.token_fallback,
            stats.lifetime.dicom_index.reused,
            index_fast.p95,
            index_token.p95,
            index_reused.p95,
            window.obsolete_source_read_ms,
            obsolete_ratio * 100.0,
            stats.lifetime.ready_cache_hits,
            stats.lifetime.in_flight_deduplications,
            stats.gauges.resident_bytes as f64 / (1024.0 * 1024.0),
            stats.gauges.pinned_bytes as f64 / (1024.0 * 1024.0),
            stats.lifetime.demand_cancellations
                + stats.lifetime.source_cancellations
                + stats.lifetime.preparation_cancellations,
            stats.lifetime.obsolete_discards,
            stats.gauges.submissions,
        ))
    }

    fn pipeline_window_json(&mut self, now: Instant) -> Option<String> {
        let stats = self.enabled.as_mut()?;
        if now.duration_since(stats.last_emitted) < EMIT_INTERVAL {
            return None;
        }
        stats.window.evict_expired(now);
        let line = pipeline_json(stats, now);
        stats.last_emitted = now;
        Some(line)
    }
}

fn safe_ratio(numerator: f64, denominator: f64) -> f64 {
    if numerator == 0.0 || denominator <= 0.0 {
        0.0
    } else {
        numerator / denominator
    }
}

fn distribution(samples: &[f64]) -> Distribution {
    Distribution {
        count: samples.len(),
        p50: percentile(samples, 0.50),
        p95: percentile(samples, 0.95),
        p99: percentile(samples, 0.99),
    }
}

fn percentile(samples: &[f64], percentile: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (percentile.clamp(0.0, 1.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use dicom_viewer_core::{
        DicomIndexDiagnostic, DicomIndexMapping, DicomIndexOutcome, LevelIndex,
    };

    use super::{
        percentile, DicomIndexDiagnosticSource, LevelPreparationStatus, PipelineStats,
        PIPELINE_SCHEMA_VERSION,
    };
    use crate::app::tile::loader::{TileBatchMetrics, TileLoaderStats, TileQueueWait};
    use crate::app::tile::QueueLane;

    #[test]
    fn percentile_reports_window_distribution_without_lifetime_averaging() {
        let samples = [40.0, 10.0, 30.0, 20.0, 50.0];

        assert_eq!(percentile(&samples, 0.50), 30.0);
        assert_eq!(percentile(&samples, 0.95), 50.0);
        assert_eq!(percentile(&samples, 0.99), 50.0);
        assert_eq!(percentile(&[], 0.95), 0.0);
    }

    #[test]
    fn empty_distributions_report_absence_instead_of_perfect_zero_latency() {
        assert_eq!(
            super::json::distribution_json(super::distribution(&[])),
            "{\"count\":0,\"p50\":null,\"p95\":null,\"p99\":null}"
        );
    }

    #[test]
    fn zero_ratios_are_serialized_with_canonical_positive_zero() {
        let ratio = super::safe_ratio(-0.0, 1.0);

        assert_eq!(ratio, 0.0);
        assert!(!ratio.is_sign_negative());
    }

    #[test]
    fn pipeline_window_keeps_a_true_trailing_second_across_emit_boundaries() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.record_app_ui_cpu_time_at(Duration::from_millis(10), started);
        stats
            .record_app_ui_cpu_time_at(Duration::from_millis(20), started + Duration::from_secs(1));
        stats.record_batch_with_obsolete_at(
            TileBatchMetrics {
                requested: 2,
                admitted: 2,
                returned: 2,
                ..TileBatchMetrics::default()
            },
            0,
            started + Duration::from_secs(1),
        );
        stats.record_cache_hit();

        let first = stats
            .pipeline_window_json(started + Duration::from_secs(1))
            .expect("one elapsed second should emit a pipeline window");
        assert!(first.contains(&format!("\"schema_version\":{PIPELINE_SCHEMA_VERSION}")));
        assert!(first.contains("\"kind\":\"pipeline_window\""));
        assert!(first.contains("\"app_ui_cpu_ms\":{\"count\":2,\"p50\":10.000"));
        assert!(first.contains("\"p95\":20.000"));
        assert!(first.contains("\"p99\":20.000"));
        assert!(first.contains("\"ready_cache_hits\":1"));

        stats.record_app_ui_cpu_time_at(
            Duration::from_millis(30),
            started + Duration::from_millis(1_500),
        );
        let second = stats
            .pipeline_window_json(started + Duration::from_secs(2))
            .expect("the next elapsed second should emit a trailing window");
        assert!(second.contains(
            "\"app_ui_cpu_ms\":{\"count\":2,\"p50\":20.000,\"p95\":30.000,\"p99\":30.000}"
        ));
        assert!(second
            .contains("\"batch_cardinality\":{\"requested\":2,\"admitted\":2,\"returned\":2}"));
        assert!(second.contains("\"ready_cache_hits\":1"));
    }

    #[test]
    fn disabled_pipeline_statistics_do_not_allocate_sample_buffers() {
        let mut stats = PipelineStats::disabled();
        stats.record_app_ui_cpu_time(Duration::from_millis(10));
        stats.record_cache_hit();

        assert!(stats.overlay_text().is_none());
        assert!(stats.enabled.is_none());
    }

    #[test]
    fn debug_stats_request_one_second_repaints_only_when_enabled() {
        let started = Instant::now();
        let stats = PipelineStats::enabled_at(started);
        assert_eq!(
            stats.next_emit_delay_at(started + Duration::from_millis(250)),
            Some(Duration::from_millis(750))
        );
        assert_eq!(
            stats.next_emit_delay_at(started + Duration::from_secs(1)),
            Some(Duration::ZERO)
        );

        let requested_after = Cell::new(None);
        stats.request_periodic_repaint(|after| requested_after.set(Some(after)));
        assert!(requested_after
            .get()
            .is_some_and(|delay| delay <= Duration::from_secs(1)));

        let disabled_request = Cell::new(false);
        PipelineStats::disabled().request_periodic_repaint(|_| disabled_request.set(true));
        assert!(!disabled_request.get());
    }

    #[test]
    fn visible_queue_wait_keeps_a_slow_tail_without_batch_averaging_or_lane_mixing() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.set_interactive_at(true, started);
        let mut queue_waits = (0..7)
            .map(|_| TileQueueWait {
                lane: QueueLane::Visible,
                milliseconds: 0.0,
            })
            .collect::<Vec<_>>();
        queue_waits.push(TileQueueWait {
            lane: QueueLane::Visible,
            milliseconds: 100.0,
        });
        queue_waits.push(TileQueueWait {
            lane: QueueLane::Prefetch,
            milliseconds: 1_000.0,
        });
        stats.record_batch_with_obsolete_at(
            TileBatchMetrics {
                queue_waits,
                requested: 9,
                admitted: 9,
                returned: 9,
                ..TileBatchMetrics::default()
            },
            0,
            started + Duration::from_millis(10),
        );

        stats.set_interactive_at(false, started + Duration::from_millis(20));
        let interaction = stats
            .observe_target_at(
                dicom_viewer_core::LevelIndex::from_u32(0),
                1,
                0,
                0,
                0,
                started + Duration::from_millis(30),
            )
            .expect("settled covered interaction should emit a summary");
        assert!(interaction.contains(
            "\"visible_queue_wait_ms\":{\"count\":8,\"p50\":0.000,\"p95\":100.000,\"p99\":100.000}"
        ));

        let window = stats
            .pipeline_window_json(started + Duration::from_secs(1))
            .expect("one elapsed second should emit a pipeline window");
        assert!(window.contains(
            "\"visible_queue_wait_ms\":{\"count\":8,\"p50\":0.000,\"p95\":100.000,\"p99\":100.000}"
        ));
        assert!(window.contains(
            "\"queue_wait_ms\":{\"count\":9,\"p50\":0.000,\"p95\":1000.000,\"p99\":1000.000}"
        ));
    }

    #[test]
    fn pipeline_window_reports_real_lane_depths_and_batch_cardinality() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.record_batch_with_obsolete(
            TileBatchMetrics {
                queue_waits: vec![TileQueueWait {
                    lane: QueueLane::Visible,
                    milliseconds: 4.0,
                }],
                source_read_ms: Some(12.0),
                requested: 8,
                admitted: 8,
                returned: 7,
                cpu_results: 5,
                metal_results: 2,
                retries: 8,
                failures: 1,
                source_cancellations: 0,
            },
            0,
        );
        stats.update_gauges(
            9,
            TileLoaderStats {
                queued: 8,
                visible: 2,
                transition: 1,
                fallback: 1,
                overview: 1,
                prefetch: 3,
                decoding: 1,
                cancellations: 2,
            },
            1024,
            256,
            3,
        );

        let line = stats
            .pipeline_window_json(started + Duration::from_secs(1))
            .expect("elapsed window should emit");
        assert!(line.contains(
            "\"queue_depth\":{\"visible\":2,\"transition\":1,\"fallback\":1,\"overview\":1,\"prefetch\":3,\"total\":8}"
        ));
        assert!(
            line.contains("\"batch_cardinality\":{\"requested\":8,\"admitted\":8,\"returned\":7}")
        );
        assert!(line.contains("\"cpu_results\":5"));
        assert!(line.contains("\"metal_results\":2"));
        assert!(line.contains("\"retries\":8"));
        assert!(line.contains("\"failures\":1"));
        assert!(line.contains("\"pinned_bytes\":256"));
    }

    #[test]
    fn interaction_summary_is_emitted_only_on_true_to_false_transition() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);

        assert!(stats.set_interactive_at(true, started).is_none());
        assert!(stats
            .set_interactive_at(true, started + Duration::from_millis(5))
            .is_none());
        stats.record_app_ui_cpu_time(Duration::from_millis(12));
        assert!(stats
            .set_interactive_at(false, started + Duration::from_millis(20))
            .is_none());
        let line = stats
            .observe_target_at(
                dicom_viewer_core::LevelIndex::from_u32(2),
                1,
                0,
                0,
                0,
                started + Duration::from_millis(25),
            )
            .expect("settled complete target coverage should emit one summary");
        assert!(line.contains("\"kind\":\"interaction_summary\""));
        assert!(line.contains("\"duration_ms\":25.000"));
        assert!(line.contains(
            "\"app_ui_cpu_ms\":{\"count\":1,\"p50\":12.000,\"p95\":12.000,\"p99\":12.000}"
        ));
        assert!(stats
            .set_interactive_at(false, started + Duration::from_millis(30))
            .is_none());
    }

    #[test]
    fn interaction_reports_first_sharp_from_gesture_start_and_coverage_after_last_input() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.record_zoom_input_at(started + Duration::from_millis(10));
        stats.set_interactive_at(true, started + Duration::from_millis(10));

        assert!(stats
            .observe_target_at(
                dicom_viewer_core::LevelIndex::from_u32(3),
                1,
                2,
                0,
                0,
                started + Duration::from_millis(30),
            )
            .is_none());
        assert!(stats
            .set_interactive_at(false, started + Duration::from_millis(40))
            .is_none());
        let line = stats
            .observe_target_at(
                dicom_viewer_core::LevelIndex::from_u32(3),
                3,
                0,
                0,
                0,
                started + Duration::from_millis(70),
            )
            .expect("summary waits for settled full target coverage");

        assert!(line.contains("\"first_sharp_target_ms\":20.000"));
        assert!(line.contains("\"full_target_coverage_ms\":60.000"));
        assert!(line.contains("\"first_sharp_origin\":\"continuous_gesture_start\""));
        assert!(line.contains("\"full_coverage_origin\":\"last_zoom_input\""));
        assert!(line.contains(
            "\"target_coverage\":{\"level\":3,\"ready\":3,\"pending\":0,\"failed\":0,\"missing\":0}"
        ));
    }

    #[test]
    fn repeated_wheel_input_does_not_reset_first_sharp_for_the_same_target_level() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.record_zoom_input_at(started + Duration::from_millis(10));
        stats.set_interactive_at(true, started + Duration::from_millis(10));
        stats.observe_target_at(
            LevelIndex::from_u32(3),
            1,
            2,
            0,
            0,
            started + Duration::from_millis(30),
        );

        stats.record_zoom_input_at(started + Duration::from_millis(40));
        stats.observe_target_at(
            LevelIndex::from_u32(3),
            1,
            2,
            0,
            0,
            started + Duration::from_millis(50),
        );
        stats.set_interactive_at(false, started + Duration::from_millis(60));
        let line = stats
            .observe_target_at(
                LevelIndex::from_u32(3),
                3,
                0,
                0,
                0,
                started + Duration::from_millis(70),
            )
            .unwrap();

        assert!(line.contains("\"first_sharp_target_ms\":20.000"));
        assert!(line.contains("\"full_target_coverage_ms\":30.000"));
    }

    #[test]
    fn transition_to_a_new_target_level_restarts_sharp_detection_not_the_gesture_clock() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.record_zoom_input_at(started + Duration::from_millis(10));
        stats.observe_target_at(
            LevelIndex::from_u32(2),
            1,
            0,
            0,
            0,
            started + Duration::from_millis(20),
        );
        stats.record_zoom_input_at(started + Duration::from_millis(25));
        stats.observe_target_at(
            LevelIndex::from_u32(3),
            0,
            2,
            0,
            0,
            started + Duration::from_millis(30),
        );
        stats.observe_target_at(
            LevelIndex::from_u32(3),
            1,
            1,
            0,
            0,
            started + Duration::from_millis(40),
        );
        stats.set_interactive_at(false, started + Duration::from_millis(50));
        let line = stats
            .observe_target_at(
                LevelIndex::from_u32(3),
                2,
                0,
                0,
                0,
                started + Duration::from_millis(60),
            )
            .unwrap();

        assert!(line.contains("\"first_sharp_target_ms\":30.000"));
        assert!(line.contains("\"full_target_coverage_ms\":35.000"));
    }

    #[test]
    fn empty_upload_plan_does_not_record_timing_but_planned_work_does() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);

        stats.record_upload_at(
            Duration::from_millis(9),
            0,
            0,
            started + Duration::from_millis(10),
        );
        assert!(stats.enabled.as_ref().unwrap().window.upload_ms.is_empty());

        stats.record_upload_at(
            Duration::from_millis(7),
            1,
            0,
            started + Duration::from_millis(20),
        );
        let upload_samples = &stats.enabled.as_ref().unwrap().window.upload_ms;
        assert_eq!(upload_samples.len(), 1);
        assert_eq!(upload_samples[0].value, 7.0);
    }

    #[test]
    fn eviction_diagnostics_report_duration_count_and_share_of_app_ui_cpu_work() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        let observed_at = started + Duration::from_millis(10);
        stats.record_app_ui_cpu_time_at(Duration::from_millis(20), observed_at);
        stats.record_eviction_at(Duration::from_millis(1), observed_at);

        let line = stats
            .pipeline_window_json(started + Duration::from_secs(1))
            .unwrap();

        assert!(line
            .contains("\"eviction_ms\":{\"count\":1,\"p50\":1.000,\"p95\":1.000,\"p99\":1.000}"));
        assert!(line.contains("\"eviction_cpu_ratio\":0.050000"));
        assert!(line.contains("\"evictions\":1"));
    }

    #[test]
    fn obsolete_source_read_time_is_attributed_once_by_obsolete_batch_fraction() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.record_zoom_input_at(started);
        stats.record_batch_with_obsolete(
            TileBatchMetrics {
                queue_waits: vec![TileQueueWait {
                    lane: QueueLane::Visible,
                    milliseconds: 2.0,
                }],
                source_read_ms: Some(40.0),
                requested: 8,
                admitted: 8,
                returned: 8,
                cpu_results: 8,
                ..TileBatchMetrics::default()
            },
            2,
        );
        stats.set_interactive_at(false, started + Duration::from_millis(50));
        let line = stats
            .observe_target_at(
                dicom_viewer_core::LevelIndex::from_u32(1),
                1,
                0,
                0,
                0,
                started + Duration::from_millis(60),
            )
            .unwrap();

        assert!(
            line.contains("\"obsolete_source_read\":{\"duration_ms\":10.000,\"ratio\":0.250000}")
        );
    }

    #[test]
    fn pipeline_window_reports_level_preparation_timing_and_outcome() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.record_level_preparation(Duration::from_millis(25), LevelPreparationStatus::Prepared);
        stats.record_level_preparation(Duration::from_millis(40), LevelPreparationStatus::Failed);
        stats
            .record_level_preparation(Duration::from_millis(10), LevelPreparationStatus::Cancelled);

        let line = stats
            .pipeline_window_json(started + Duration::from_secs(1))
            .expect("elapsed window should emit");
        assert!(line.contains(
            "\"level_preparation_ms\":{\"count\":3,\"p50\":25.000,\"p95\":40.000,\"p99\":40.000}"
        ));
        assert!(line.contains("\"level_preparations\":1"));
        assert!(line.contains("\"level_preparation_failures\":1"));
        assert!(line.contains("\"preparation\":1"));
    }

    #[test]
    fn pipeline_window_reports_typed_dicom_index_outcomes_and_mappings() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.record_dicom_index_diagnostics(
            DicomIndexDiagnosticSource::Read,
            &[
                DicomIndexDiagnostic::new(
                    DicomIndexOutcome::BuiltFast {
                        mapping: DicomIndexMapping::BasicOffsetTableItems,
                    },
                    Duration::from_millis(12),
                ),
                DicomIndexDiagnostic::new(
                    DicomIndexOutcome::FastPathFallback,
                    Duration::from_millis(3),
                ),
                DicomIndexDiagnostic::new(
                    DicomIndexOutcome::TokenFallback,
                    Duration::from_millis(20),
                ),
                DicomIndexDiagnostic::new(DicomIndexOutcome::Reused, Duration::from_millis(1)),
            ],
        );

        let line = stats
            .pipeline_window_json(started + Duration::from_secs(1))
            .expect("elapsed window should emit");
        assert!(line.contains("\"built_fast_ms\":{\"count\":1,\"p50\":12.000,\"p95\":12.000"));
        assert!(line.contains("\"fast_path_fallback_ms\":{\"count\":1,\"p50\":3.000"));
        assert!(line.contains("\"token_fallback_ms\":{\"count\":1,\"p50\":20.000"));
        assert!(line.contains("\"reused_ms\":{\"count\":1,\"p50\":1.000"));
        assert!(line.contains("\"built_fast\":1"));
        assert!(line.contains("\"fast_path_fallback\":1"));
        assert!(line.contains("\"token_fallback\":1"));
        assert!(line.contains("\"reused\":1"));
        assert!(line.contains("\"basic_offset_table_items\":1"));
        assert!(line.contains("\"source\":{\"preparation\":0,\"read\":4}"));
    }

    #[test]
    fn interaction_summary_attributes_index_events_to_preparation() {
        let started = Instant::now();
        let mut stats = PipelineStats::enabled_at(started);
        stats.record_zoom_input_at(started);
        stats.record_dicom_index_diagnostics_at(
            DicomIndexDiagnosticSource::Preparation,
            &[DicomIndexDiagnostic::new(
                DicomIndexOutcome::BuiltFast {
                    mapping: DicomIndexMapping::ExtendedOffsetTableDirect,
                },
                Duration::from_millis(14),
            )],
            started + Duration::from_millis(10),
        );
        stats.set_interactive_at(false, started + Duration::from_millis(20));

        let line = stats
            .observe_target_at(
                dicom_viewer_core::LevelIndex::from_u32(1),
                1,
                0,
                0,
                0,
                started + Duration::from_millis(30),
            )
            .expect("settled complete coverage should emit a summary");
        assert!(line.contains("\"built_fast_ms\":{\"count\":1,\"p50\":14.000"));
        assert!(line.contains("\"interaction\":{\"source\":{\"preparation\":1,\"read\":0}"));
        assert!(line.contains("\"extended_offset_table_direct\":1"));
    }
}
