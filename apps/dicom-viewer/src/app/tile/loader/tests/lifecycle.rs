use super::*;

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
    loader.enqueue_or_reprioritize_batch(vec![request(&shared_study, 0, QueueLane::Visible, 0)]);
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
    {
        let state = lock_state(&loader.shared);
        assert!(state.decoding.contains_key(&tile));
        assert!(state.in_flight_decoded_bytes > 0);
    }

    loader.acknowledge_finished(batch_id, &keys);

    let state = lock_state(&loader.shared);
    assert!(!state.decoding.contains_key(&tile));
    assert_eq!(state.in_flight_decoded_bytes, 0);
}

#[test]
fn disabled_metrics_capture_no_enqueue_timestamps() {
    let loader = TileLoader::with_worker_count(0);
    let shared_study = study();
    loader.enqueue_or_reprioritize_batch(vec![request(&shared_study, 0, QueueLane::Visible, 0)]);

    assert!(lock_state(&loader.shared).queued[&key(0)]
        .enqueued_at
        .is_none());
}

#[test]
fn enabled_metrics_capture_enqueue_timestamps() {
    let loader = TileLoader::with_worker_count_and_metrics(0, true, usize::MAX);
    let shared_study = study();
    loader.enqueue_or_reprioritize_batch(vec![request(&shared_study, 0, QueueLane::Visible, 0)]);

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
        let loader = TileLoader::with_worker_count_and_metrics(0, metrics_enabled, usize::MAX);
        let shared_study = study();
        loader.enqueue_or_reprioritize_batch(vec![request(
            &shared_study,
            0,
            QueueLane::Visible,
            0,
        )]);
        let batch = wait_for_batch(&loader.shared).expect("queued tile should form a batch");
        batch.control.record_diagnostic(diagnostic);

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
