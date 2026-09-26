use super::*;

#[test]
#[ignore = "manual release scheduler characterization; requires DICOM_VIEWER_WSI_FIXTURE"]
fn compatible_queue_release_characterization() {
    if cfg!(debug_assertions) {
        panic!("run this characterization with --release");
    }
    let path =
        std::env::var_os("DICOM_VIEWER_WSI_FIXTURE").expect("a local WSI fixture is required");
    let study = Arc::new(
        ViewerStudy::open_path_with_options(path, dicom_viewer_core::ViewerOpenOptions::cpu_only())
            .unwrap(),
    );
    let level = &study.summary().levels[0];
    let (columns, rows) = level.tile_layout.grid_size().unwrap();
    let count = columns.saturating_mul(rows).min(8_192);
    assert!(
        count >= 16,
        "scheduler fixture must have at least 16 real tiles"
    );
    let key_at = |index| TileKey {
        generation: 1,
        level: level.index,
        coord: TileCoord::new(index % columns, index / columns),
    };
    let expected = std::iter::once(key_at(0))
        .chain((count - 7..count).map(key_at))
        .collect::<Vec<_>>();
    for sample in 1..=15 {
        let mut state = LoaderState::default();
        for index in 0..count {
            let key = key_at(index);
            let priority = priority(QueueLane::Visible, u128::from(index), index);
            let read_mode = if index == 0 || index >= count - 7 {
                TileReadMode::Preferred
            } else {
                TileReadMode::CpuFallback
            };
            state.queued.insert(
                key,
                QueuedJob {
                    priority,
                    read_mode,
                    enqueued_at: None,
                },
            );
            state.jobs.push(Reverse(TileJob {
                key,
                priority,
                read_mode,
                enqueued_at: None,
                study: Arc::clone(&study),
            }));
        }
        let start = Instant::now();
        let batch = std::hint::black_box(pop_next_batch(&mut state).unwrap());
        let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(
            batch.iter().map(|job| job.key).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(state.queued.len(), count as usize - 8);
        eprintln!(
            "{}",
            serde_json::json!({"workload":"compatible-queue", "sample":sample,"jobs":count,"batch":batch.len(),"elapsed_ms":elapsed_ms})
        );
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
fn compatible_batch_defers_other_modes_without_losing_byte_budget_or_priority() {
    let study = study();
    let mut state = LoaderState {
        max_in_flight_decoded_bytes: 4 * 16 * 16 * 8,
        ..LoaderState::default()
    };
    let requests = (0..16)
        .map(|col| {
            let mut job = request(&study, col, QueueLane::Visible, col);
            if col > 0 && col < 9 {
                job.read_mode = TileReadMode::CpuFallback;
            }
            job
        })
        .collect();
    enqueue_requests(&mut state, requests, true);
    let first = pop_next_batch(&mut state).unwrap();
    assert_eq!(
        first.iter().map(|job| job.key).collect::<Vec<_>>(),
        [0, 9, 10, 11].map(key)
    );
    let next = pop_next_batch(&mut state).unwrap();
    assert_eq!(
        next.iter().map(|job| job.key).collect::<Vec<_>>(),
        [1, 2, 3, 4].map(key)
    );
    assert!(next
        .iter()
        .all(|job| job.read_mode == TileReadMode::CpuFallback));
    assert_eq!(state.queued.len(), 8);
}

#[test]
fn decoded_byte_budget_caps_each_batch_and_all_concurrent_batches() {
    let shared_study = study();
    let tile_bytes = 16 * 16 * 8;
    let mut state = LoaderState {
        max_in_flight_decoded_bytes: tile_bytes * 2,
        ..LoaderState::default()
    };
    for col in 0..8 {
        let tile = key(col);
        let tile_priority = priority(QueueLane::Visible, u128::from(col), col);
        state.queued.insert(
            tile,
            QueuedJob {
                priority: tile_priority,
                read_mode: TileReadMode::Preferred,
                enqueued_at: None,
            },
        );
        state.jobs.push(Reverse(TileJob {
            key: tile,
            priority: tile_priority,
            study: Arc::clone(&shared_study),
            read_mode: TileReadMode::Preferred,
            enqueued_at: None,
        }));
    }

    let first = pop_next_batch(&mut state).unwrap();
    assert_eq!(first.len(), 2);
    state.in_flight_decoded_bytes = tile_bytes * first.len();
    assert!(pop_next_batch(&mut state).is_none());

    state.in_flight_decoded_bytes = 0;
    assert_eq!(pop_next_batch(&mut state).unwrap().len(), 2);
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
                reserved_decoded_bytes: 0,
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
