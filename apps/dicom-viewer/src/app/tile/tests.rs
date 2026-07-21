use dicom_viewer_core::{LevelIndex, TileCoord};

use super::*;

fn key(generation: u64) -> TileKey {
    tile(generation, 0).key
}

fn tile(generation: u64, col: u64) -> VisibleTile {
    let key = TileKey {
        generation,
        level: LevelIndex::from_u32(0),
        coord: TileCoord::new(col, 0),
    };
    VisibleTile {
        key,
        distance2: u128::from(col),
    }
}

fn upload_request<'a>(
    relevant_tiles: &'a HashSet<TileKey>,
    visible: &'a [VisibleTile],
    transition: &'a [VisibleTile],
    fallback: &'a [VisibleTile],
    overview: &'a [VisibleTile],
    prefetch: &'a [VisibleTile],
    interactive: bool,
) -> TilePollRequest<'a> {
    TilePollRequest {
        generation: 1,
        relevant_tiles,
        visible_tiles: visible,
        transition_target_tiles: transition,
        fallback_tiles: fallback,
        overview_tiles: overview,
        background_prefetch_tiles: prefetch,
        render_level_index: LevelIndex::from_u32(0),
        interactive,
    }
}

fn decoded_keys(lanes: &[&[VisibleTile]]) -> HashSet<TileKey> {
    lanes
        .iter()
        .flat_map(|tiles| tiles.iter().map(|tile| tile.key))
        .collect()
}

fn lane_count(plan: &UploadPlan, lane: QueueLane) -> usize {
    plan.entries
        .iter()
        .filter(|entry| entry.lane == lane)
        .count()
}

fn assert_lane_order(plan: &UploadPlan, expected: &[(QueueLane, usize)]) {
    let actual = plan
        .entries
        .iter()
        .map(|entry| entry.lane)
        .collect::<Vec<_>>();
    let expected = expected
        .iter()
        .flat_map(|(lane, count)| std::iter::repeat_n(*lane, *count))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn queue_lanes_keep_current_view_ahead_of_transition_and_prefetch() {
    assert!(QueueLane::Visible < QueueLane::TransitionTarget);
    assert!(QueueLane::TransitionTarget < QueueLane::Fallback);
    assert!(QueueLane::Fallback < QueueLane::Overview);
    assert!(QueueLane::Overview < QueueLane::Prefetch);
}

#[test]
fn interactive_target_tiles_do_not_wait_for_zoom_to_settle() {
    assert_eq!(prefetch_queue_lane(true), QueueLane::TransitionTarget);
}

#[test]
fn idle_prefetch_waits_until_visible_tiles_are_ready() {
    assert_eq!(prefetch_queue_lane(false), QueueLane::Prefetch);
}

#[test]
fn tile_key_generation_is_the_only_stale_result_identity() {
    assert!(!is_stale_job(key(3), 3));
    assert!(is_stale_job(key(3), 4));
}

#[test]
fn edge_tile_texture_preflight_uses_checked_final_rgba_bytes() {
    let level = LevelInfo {
        index: LevelIndex::from_u32(0),
        width: 3,
        height: 2,
        downsample: 1.0,
        tile_layout: dicom_viewer_core::LevelTileLayout::Regular {
            tile_width: 2,
            tile_height: 2,
            tiles_across: 2,
            tiles_down: 1,
        },
    };

    assert_eq!(planned_texture_bytes(&level, TileCoord::new(0, 0)), Ok(16));
    assert_eq!(planned_texture_bytes(&level, TileCoord::new(1, 0)), Ok(8));
}

#[test]
fn tile_texture_preflight_fails_closed_on_coordinate_arithmetic_overflow() {
    let level = LevelInfo {
        index: LevelIndex::from_u32(0),
        width: u64::MAX,
        height: 1,
        downsample: 1.0,
        tile_layout: dicom_viewer_core::LevelTileLayout::Regular {
            tile_width: u32::MAX,
            tile_height: 1,
            tiles_across: u64::MAX,
            tiles_down: 1,
        },
    };

    let error = planned_texture_bytes(&level, TileCoord::new(u64::MAX, 0)).unwrap_err();
    assert!(error.contains("overflow"), "{error}");
}

#[test]
fn loader_metrics_accept_only_batches_wholly_owned_by_the_active_study() {
    assert!(loader_batch_belongs_to_generation(
        3,
        &[key(3), tile(3, 1).key]
    ));
    assert!(!loader_batch_belongs_to_generation(3, &[key(2)]));
    assert!(!loader_batch_belongs_to_generation(3, &[key(3), key(2)]));
    assert!(!loader_batch_belongs_to_generation(3, &[]));
}

#[test]
fn upload_plan_reserves_two_transition_slots_from_the_24_foreground_slots() {
    let visible = (0..30).map(|col| tile(1, col)).collect::<Vec<_>>();
    let transition = (100..102).map(|col| tile(1, col)).collect::<Vec<_>>();
    let relevant = decoded_keys(&[&visible, &transition]);
    let request = upload_request(&relevant, &visible, &transition, &[], &[], &[], true);

    let plan = assemble_upload_plan(&request, &relevant, &relevant, false, false);

    assert_eq!(plan.entries.len(), 24);
    assert_lane_order(
        &plan,
        &[(QueueLane::Visible, 22), (QueueLane::TransitionTarget, 2)],
    );
    assert_eq!(plan.cpu_budget, VISIBLE_UPLOAD_BUDGET);
}

#[test]
fn unused_transition_reservation_returns_to_visible_uploads() {
    let visible = (0..30).map(|col| tile(1, col)).collect::<Vec<_>>();
    let transition = [tile(1, 100)];
    let relevant = decoded_keys(&[&visible, &transition]);
    let request = upload_request(&relevant, &visible, &transition, &[], &[], &[], true);

    let plan = assemble_upload_plan(&request, &relevant, &relevant, false, false);

    assert_eq!(lane_count(&plan, QueueLane::Visible), 23);
    assert_eq!(lane_count(&plan, QueueLane::TransitionTarget), 1);
    assert_eq!(plan.entries.len(), 24);
}

#[test]
fn foreground_upload_order_is_visible_then_transition_then_fallback() {
    let visible = (0..2).map(|col| tile(1, col)).collect::<Vec<_>>();
    let transition = (10..12).map(|col| tile(1, col)).collect::<Vec<_>>();
    let fallback = (20..50).map(|col| tile(1, col)).collect::<Vec<_>>();
    let relevant = decoded_keys(&[&visible, &transition, &fallback]);
    let request = upload_request(&relevant, &visible, &transition, &fallback, &[], &[], true);

    let plan = assemble_upload_plan(&request, &relevant, &relevant, true, false);

    assert_eq!(plan.entries.len(), 24);
    assert_lane_order(
        &plan,
        &[
            (QueueLane::Visible, 2),
            (QueueLane::TransitionTarget, 2),
            (QueueLane::Fallback, 20),
        ],
    );
}

#[test]
fn upload_plan_deduplicates_a_key_to_its_highest_lane() {
    let shared = tile(1, 5);
    let relevant = HashSet::from([shared.key]);
    let request = upload_request(
        &relevant,
        std::slice::from_ref(&shared),
        std::slice::from_ref(&shared),
        std::slice::from_ref(&shared),
        std::slice::from_ref(&shared),
        std::slice::from_ref(&shared),
        true,
    );

    let plan = assemble_upload_plan(&request, &relevant, &relevant, true, false);

    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].lane, QueueLane::Visible);
}

#[test]
fn idle_background_uploads_overview_then_prefetch_with_an_aggregate_cap_of_four() {
    let overview = (0..3).map(|col| tile(1, col)).collect::<Vec<_>>();
    let prefetch = (10..13).map(|col| tile(1, col)).collect::<Vec<_>>();
    let relevant = decoded_keys(&[&overview, &prefetch]);
    let request = upload_request(&relevant, &[], &[], &[], &overview, &prefetch, false);

    let idle = assemble_upload_plan(&request, &relevant, &relevant, false, true);
    let foreground_busy = assemble_upload_plan(&request, &relevant, &relevant, false, false);

    assert_lane_order(&idle, &[(QueueLane::Overview, 3), (QueueLane::Prefetch, 1)]);
    assert_eq!(idle.cpu_budget, PREFETCH_UPLOAD_BUDGET);
    assert!(foreground_busy.entries.is_empty());
}

#[test]
fn failed_only_foreground_allows_idle_background_uploads() {
    let failed = [tile(1, 0)];
    let overview = [tile(1, 10)];
    let relevant = decoded_keys(&[&overview]);
    let request = upload_request(&relevant, &failed, &[], &[], &overview, &[], false);

    let plan = assemble_upload_plan(&request, &relevant, &relevant, true, true);

    assert_lane_order(&plan, &[(QueueLane::Overview, 1)]);
}

#[test]
fn canonical_frame_demand_deduplicates_keys_to_the_highest_lane() {
    let tile = VisibleTile {
        key: key(3),
        distance2: 9,
    };
    let same_tile_nearer = VisibleTile {
        key: key(3),
        distance2: 1,
    };
    let relevant = HashSet::from([tile.key]);
    let demand = FrameTileDemand {
        study_generation: 3,
        current_visible: std::slice::from_ref(&tile),
        transition_target: std::slice::from_ref(&same_tile_nearer),
        fallback_visible: &[],
        overview: &[],
        background_prefetch: &[],
        cache_relevant: &relevant,
    };

    let canonical = canonical_frame_requests(&demand);

    assert_eq!(canonical.len(), 1);
    assert_eq!(canonical[&tile.key], (QueueLane::Visible, 9));
}

fn request_entry(col: u64, lane: QueueLane, distance2: u128) -> (TileKey, (QueueLane, u128)) {
    (tile(1, col).key, (lane, distance2))
}

fn decoded_result(key: TileKey) -> TileLoadResult {
    TileLoadResult {
        key,
        outcome: TileLoadOutcome::Decoded(DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        })),
        used_cpu_fallback: false,
    }
}

#[test]
fn frame_demand_cap_drops_farthest_prefetch_then_fallback_then_transition() {
    let requests = HashMap::from([
        request_entry(0, QueueLane::Visible, 99),
        request_entry(1, QueueLane::Overview, 99),
        request_entry(2, QueueLane::TransitionTarget, 1),
        request_entry(3, QueueLane::TransitionTarget, 9),
        request_entry(4, QueueLane::Fallback, 1),
        request_entry(5, QueueLane::Fallback, 9),
        request_entry(6, QueueLane::Prefetch, 1),
        request_entry(7, QueueLane::Prefetch, 9),
    ]);

    let seven = cap_frame_requests_to(requests.clone(), 7);
    assert!(!seven.requests.contains_key(&tile(1, 7).key));
    assert!(seven.requests.contains_key(&tile(1, 6).key));

    let six = cap_frame_requests_to(requests.clone(), 6);
    assert!(!six.requests.contains_key(&tile(1, 6).key));
    assert!(six.requests.contains_key(&tile(1, 5).key));

    let five = cap_frame_requests_to(requests, 5);
    assert!(!five.requests.contains_key(&tile(1, 5).key));
    assert!(five.requests.contains_key(&tile(1, 4).key));
    assert!(five.requests.contains_key(&tile(1, 3).key));
    assert!(five.requests.contains_key(&tile(1, 1).key));
}

#[test]
fn visible_overflow_retains_only_the_nearest_visible_tiles() {
    let requests = HashMap::from([
        request_entry(0, QueueLane::Visible, 90),
        request_entry(1, QueueLane::Visible, 3),
        request_entry(2, QueueLane::Visible, 1),
        request_entry(3, QueueLane::Visible, 20),
        request_entry(4, QueueLane::Visible, 2),
        request_entry(5, QueueLane::Overview, 0),
    ]);

    let capped = cap_frame_requests_to(requests, 3);

    assert_eq!(capped.visible_requested, 5);
    assert_eq!(capped.visible_dropped, 2);
    assert_eq!(capped.requests.len(), 3);
    assert_eq!(
        capped.requests.keys().copied().collect::<HashSet<_>>(),
        HashSet::from([tile(1, 1).key, tile(1, 2).key, tile(1, 4).key])
    );
}

#[test]
fn capped_keep_set_prunes_old_over_cap_queued_work() {
    use super::store::QueueStatus;

    let requests = HashMap::from([
        request_entry(0, QueueLane::Visible, 0),
        request_entry(1, QueueLane::Overview, 0),
        request_entry(2, QueueLane::Prefetch, 1),
        request_entry(3, QueueLane::Prefetch, 2),
    ]);
    let decoded_cache_key = tile(1, 4).key;
    let mut full_cache_relevant = requests.keys().copied().collect::<HashSet<_>>();
    full_cache_relevant.insert(decoded_cache_key);
    let capped = cap_frame_requests_to(requests, 2);
    let keep = planned_request_keys(&capped.requests);
    let mut store = TileStore::new(1024);
    for key in &full_cache_relevant {
        assert!(matches!(store.queue(*key), QueueStatus::New));
    }
    store.stage_finished(1, &full_cache_relevant, decoded_result(decoded_cache_key));

    store.retain_relevant_pending_tiles(&keep);

    assert_eq!(store.loading_count(), 3);
    assert!(store.is_decoded(decoded_cache_key));
    assert!(matches!(
        store.queue(tile(1, 0).key),
        QueueStatus::Reprioritize
    ));
    assert!(matches!(store.queue(tile(1, 2).key), QueueStatus::New));
    assert!(
        full_cache_relevant.contains(&tile(1, 2).key),
        "the dropped queued tile remains useful cache state, but must not remain planned"
    );
}

#[test]
fn loading_spinner_counts_only_work_in_the_active_capped_demand() {
    use super::store::QueueStatus;

    let active_queued = tile(1, 0).key;
    let active_decoded = tile(1, 1).key;
    let inactive_decoded = tile(1, 2).key;
    let all = HashSet::from([active_queued, active_decoded, inactive_decoded]);
    let active = HashSet::from([active_queued, active_decoded]);
    let mut store = TileStore::new(1024);
    for &key in &all {
        assert!(matches!(store.queue(key), QueueStatus::New));
    }
    store.stage_finished(1, &all, decoded_result(active_decoded));
    store.stage_finished(1, &all, decoded_result(inactive_decoded));

    assert_eq!(store.loading_count(), 3);
    assert_eq!(store.loading_count_for(&active), 2);
}

#[test]
fn capped_result_relevance_discards_pruned_work_but_accepts_pinned_overview() {
    let visible = tile(1, 0).key;
    let pruned = tile(1, 1).key;
    let pinned_overview = tile(1, 2).key;
    let capped = cap_frame_requests_to(
        HashMap::from([
            (visible, (QueueLane::Visible, 0)),
            (pruned, (QueueLane::Fallback, 1)),
            (pinned_overview, (QueueLane::Overview, 0)),
        ]),
        1,
    );
    let result_keys = accepted_result_keys(
        &capped.planned_keys,
        std::slice::from_ref(&VisibleTile {
            key: pinned_overview,
            distance2: 0,
        }),
        1,
    );
    let mut store = TileStore::new(1024);
    store.set_pinned(HashSet::from([pruned, pinned_overview]));
    store.queue(pruned);
    store.mark_decoding(&[pruned], 1);

    assert!(stage_loader_result(
        &mut store,
        1,
        &result_keys,
        decoded_result(pruned),
    ));
    assert!(!store.is_decoded(pruned));

    store.queue(pinned_overview);
    store.mark_decoding(&[pinned_overview], 1);
    assert!(!stage_loader_result(
        &mut store,
        1,
        &result_keys,
        decoded_result(pinned_overview),
    ));
    assert!(store.is_decoded(pinned_overview));
}

#[test]
fn obsolete_batch_completion_does_not_mutate_replacement_tile_state() {
    use super::store::QueueStatus;

    let a = tile(1, 0).key;
    let accepted = HashSet::from([a]);
    let mut store = TileStore::new(1024);
    assert!(matches!(store.queue(a), QueueStatus::New));
    store.mark_decoding(&[a], 1);

    assert!(stage_loader_result_if_current(
        &mut store,
        1,
        &accepted,
        false,
        TileLoadResult {
            key: a,
            outcome: TileLoadOutcome::Cancelled,
            used_cpu_fallback: false,
        },
    ));
    assert_eq!(
        store.queue_for_demand(a),
        TileDemandStatus::Deduplicated,
        "an obsolete completion must leave replacement decoding state untouched"
    );
    assert!(stage_loader_result_if_current(
        &mut store,
        1,
        &accepted,
        false,
        TileLoadResult {
            key: a,
            outcome: TileLoadOutcome::Failed(TileFailure::new("obsolete failure")),
            used_cpu_fallback: false,
        },
    ));
    assert_eq!(store.queue_for_demand(a), TileDemandStatus::Deduplicated);
}

#[test]
fn older_success_is_accepted_when_its_key_is_demanded_again() {
    use super::store::QueueStatus;

    let a = tile(1, 0).key;
    let accepted = HashSet::from([a]);
    let mut store = TileStore::new(1024);
    assert!(matches!(store.queue(a), QueueStatus::New));
    store.mark_decoding(&[a], 1);

    assert!(!stage_loader_result_if_current(
        &mut store,
        1,
        &accepted,
        false,
        decoded_result(a),
    ));
    assert!(store.is_decoded(a));
}

#[test]
fn pruned_decoded_tiles_are_not_admitted_to_the_upload_plan() {
    let prefetch = [tile(1, 0), tile(1, 1)];
    let decoded = decoded_keys(&[&prefetch]);
    let eligible = HashSet::from([prefetch[0].key]);
    let request = upload_request(&decoded, &[], &[], &[], &[], &prefetch, false);

    let plan = assemble_upload_plan(&request, &decoded, &eligible, false, true);

    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].key, prefetch[0].key);
}

#[test]
fn last_ready_upload_requests_a_followup_frame_for_background_demand() {
    assert!(poll_requires_followup(1, 1, 0));
}

#[test]
fn last_failed_result_requests_a_followup_frame_for_background_demand() {
    assert!(poll_requires_followup(1, 0, 1));
}

#[test]
fn demand_epoch_changes_only_when_key_to_lane_membership_changes() {
    let tile = key(3);
    let mut epoch = DemandEpoch::INITIAL;
    let mut current = HashMap::new();

    update_demand_epoch(
        &mut epoch,
        &mut current,
        HashMap::from([(tile, QueueLane::Visible)]),
    );
    assert_eq!(epoch, DemandEpoch(1));
    update_demand_epoch(
        &mut epoch,
        &mut current,
        HashMap::from([(tile, QueueLane::Visible)]),
    );
    assert_eq!(epoch, DemandEpoch(1));
    update_demand_epoch(
        &mut epoch,
        &mut current,
        HashMap::from([(tile, QueueLane::TransitionTarget)]),
    );
    assert_eq!(epoch, DemandEpoch(2));
}
