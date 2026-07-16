use dicom_viewer_core::{LevelIndex, TileCoord};

use super::*;

fn key(generation: u64) -> TileKey {
    TileKey {
        generation,
        level: LevelIndex::from_u32(0),
        coord: TileCoord::new(0, 0),
    }
}

#[test]
fn queue_lanes_encode_fallback_visible_prefetch_order() {
    assert!(QueueLane::Fallback < QueueLane::Visible);
    assert!(QueueLane::Visible < QueueLane::Prefetch);
}

#[test]
fn tile_key_generation_is_the_only_stale_result_identity() {
    assert!(!is_stale_job(key(3), 3));
    assert!(is_stale_job(key(3), 4));
}
