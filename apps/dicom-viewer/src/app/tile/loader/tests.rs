mod config;
mod decode;
mod demand;
mod lifecycle;
mod queue;

use std::sync::Barrier;
use std::time::Duration;

use dicom_viewer_core::{
    DicomIndexDiagnostic, DicomIndexMapping, DicomIndexOutcome, LevelIndex, TileCoord,
};

use super::super::QueueLane;
use super::*;

// `j2k-test-support` is not published; generate the one fixture shape these
// tests need through the crates.io `j2k-native` encoder.
fn htj2k_rgb8_fixture(width: u32, height: u32) -> Vec<u8> {
    let pixels = (0u32..width * height * 3)
        .map(|index| ((index * 13 + index / 3) & 0xff) as u8)
        .collect::<Vec<_>>();
    let options = j2k_native::EncodeOptions {
        reversible: true,
        num_decomposition_levels: 1,
        ..j2k_native::EncodeOptions::default()
    };
    j2k_native::encode_htj2k(&pixels, width, height, 3, 8, false, &options)
        .expect("encode HTJ2K fixture")
}
fn study() -> Arc<ViewerStudy> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 16)).unwrap();
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

fn request(
    shared_study: &Arc<ViewerStudy>,
    col: u64,
    lane: QueueLane,
    sequence: u64,
) -> QueuedTileRequest {
    QueuedTileRequest {
        study: Arc::clone(shared_study),
        key: key(col),
        priority: priority(lane, u128::from(col), sequence),
        read_mode: TileReadMode::Preferred,
    }
}
