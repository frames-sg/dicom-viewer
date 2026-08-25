use super::*;

pub(super) fn enqueue_requests(
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

pub(super) fn wait_for_batch(shared: &LoaderShared) -> Option<TileBatch> {
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
            let reserved_decoded_bytes = jobs
                .iter()
                .map(decoded_byte_reservation)
                .try_fold(0_usize, usize::checked_add)
                .expect("admitted tile decode reservations must fit in usize");
            state.in_flight_decoded_bytes = state
                .in_flight_decoded_bytes
                .checked_add(reserved_decoded_bytes)
                .expect("in-flight tile decode reservations must fit in usize");
            debug_assert!(state.in_flight_decoded_bytes <= state.max_in_flight_decoded_bytes);
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
                control = control.with_diagnostic_sink(Arc::new(move |diagnostic| {
                    captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(diagnostic);
                }));
            }
            state.in_flight.insert(
                id,
                InFlightBatch {
                    demand_epoch,
                    lane,
                    keys,
                    reserved_decoded_bytes,
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

pub(super) fn take_index_diagnostics(
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

pub(super) fn pop_next_batch(state: &mut LoaderState) -> Option<Vec<TileJob>> {
    let available_bytes = state
        .max_in_flight_decoded_bytes
        .saturating_sub(state.in_flight_decoded_bytes);
    if available_bytes == 0 {
        return None;
    }
    let lane = choose_next_lane(state)?;
    let first = pop_next_valid_job_for_lane(state, lane, available_bytes)?;
    let mut reserved_bytes = decoded_byte_reservation(&first);
    let mut jobs = vec![first];
    while jobs.len() < state.max_batch_size {
        let remaining_bytes = available_bytes.saturating_sub(reserved_bytes);
        let Some(job) = pop_next_compatible_job(state, &jobs[0], remaining_bytes) else {
            break;
        };
        reserved_bytes = reserved_bytes
            .checked_add(decoded_byte_reservation(&job))
            .expect("admitted tile decode reservations must fit in usize");
        jobs.push(job);
    }
    Some(jobs)
}

fn decoded_byte_reservation(job: &TileJob) -> usize {
    let Some(level) = job
        .study
        .summary()
        .levels
        .iter()
        .find(|level| level.index == job.key.level)
    else {
        return usize::MAX;
    };
    let (width, height) = level.tile_layout.display_tile_size();
    if width == 0 || height == 0 {
        return usize::MAX;
    }
    super::super::TileFootprint::for_level_tile(level, job.key.coord)
        .map(super::super::TileFootprint::in_flight_reservation_bytes)
        .unwrap_or_else(|_| {
            super::super::TileFootprint::for_rgba_dimensions(width, height)
                .map(super::super::TileFootprint::in_flight_reservation_bytes)
                .unwrap_or(usize::MAX)
        })
}

pub(super) fn choose_next_lane(state: &LoaderState) -> Option<QueueLane> {
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

fn pop_next_valid_job_for_lane(
    state: &mut LoaderState,
    lane: QueueLane,
    available_bytes: usize,
) -> Option<TileJob> {
    let mut deferred = Vec::new();
    while let Some(Reverse(job)) = state.jobs.pop() {
        if !is_current_job(state, &job) {
            continue;
        }
        if job.priority.lane == lane && decoded_byte_reservation(&job) <= available_bytes {
            state.queued.remove(&job.key);
            state.jobs.extend(deferred.into_iter().map(Reverse));
            return Some(job);
        }
        deferred.push(job);
    }
    state.jobs.extend(deferred.into_iter().map(Reverse));
    None
}

fn pop_next_compatible_job(
    state: &mut LoaderState,
    first: &TileJob,
    available_bytes: usize,
) -> Option<TileJob> {
    let mut deferred = Vec::new();
    while let Some(Reverse(job)) = state.jobs.pop() {
        if !is_current_job(state, &job) {
            continue;
        }
        if can_batch(first, &job) && decoded_byte_reservation(&job) <= available_bytes {
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

pub(super) fn finish_batch(shared: &LoaderShared, batch_id: u64, keys: &[TileKey]) -> Vec<TileKey> {
    let mut state = lock_state(shared);
    if let Some(batch) = state.in_flight.remove(&batch_id) {
        state.in_flight_decoded_bytes = state
            .in_flight_decoded_bytes
            .saturating_sub(batch.reserved_decoded_bytes);
    }
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

pub(super) fn cancel_in_flight(state: &LoaderState) -> u64 {
    let mut cancelled = 0;
    for batch in state.in_flight.values() {
        if !batch.token.is_cancelled() {
            batch.token.cancel();
            cancelled += 1;
        }
    }
    cancelled
}

pub(super) fn lock_state(shared: &LoaderShared) -> std::sync::MutexGuard<'_, LoaderState> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
