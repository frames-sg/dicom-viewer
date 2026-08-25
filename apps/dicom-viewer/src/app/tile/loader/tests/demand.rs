use super::*;

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
