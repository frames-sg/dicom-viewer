use super::*;

#[test]
#[ignore = "release worker/batch matrix; requires DICOM_VIEWER_WSI_FIXTURE"]
fn loader_cpu_performance() {
    if cfg!(debug_assertions) {
        panic!("run with --release");
    }
    let path = std::env::var_os("DICOM_VIEWER_WSI_FIXTURE").expect("trusted local WSI fixture");
    let study = Arc::new(
        ViewerStudy::open_path_with_options(path, dicom_viewer_core::ViewerOpenOptions::cpu_only())
            .unwrap(),
    );
    let level = &study.summary().levels[0];
    let (cols, rows) = level.tile_layout.grid_size().unwrap();
    assert!(cols >= 8 && rows >= 8);
    let requests: Vec<_> = (0..64)
        .map(|index| {
            (
                level.index,
                TileCoord::new((index % 8) * (cols - 1) / 7, (index / 8) * (rows - 1) / 7),
            )
        })
        .collect();
    let oracle = study.read_tiles_rgba(&requests).unwrap();
    let threads = std::env::var("DICOM_VIEWER_JP2K_THREADS").unwrap_or_else(|_| "default".into());
    for workers in [1, 2, 4] {
        for batch in [1, 4, 8, 16] {
            for sample in 0..5 {
                let loader =
                    TileLoader::with_worker_count_and_metrics(workers, false, 32 * 1024 * 1024);
                lock_state(&loader.shared).max_batch_size = batch;
                let queued = requests
                    .iter()
                    .enumerate()
                    .map(|(index, &(level, coord))| QueuedTileRequest {
                        study: Arc::clone(&study),
                        key: TileKey {
                            generation: 1,
                            level,
                            coord,
                        },
                        priority: priority(QueueLane::Visible, index as u128, index as u64),
                        read_mode: TileReadMode::Preferred,
                    })
                    .collect();
                let mut completed = Vec::with_capacity(requests.len());
                let started = Instant::now();
                loader.enqueue_or_reprioritize_batch(queued);
                while completed.len() < requests.len() {
                    let message = loader
                        .receiver
                        .as_ref()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(30))
                        .unwrap();
                    if let TileLoaderMessage::Finished {
                        batch_id, results, ..
                    } = message
                    {
                        let keys: Vec<_> = results.iter().map(|result| result.key).collect();
                        loader.acknowledge_finished(batch_id, &keys);
                        completed.extend(results);
                    }
                }
                let elapsed = started.elapsed();
                assert_eq!(lock_state(&loader.shared).in_flight_decoded_bytes, 0);
                for result in completed {
                    let index = requests
                        .iter()
                        .position(|&(level, coord)| {
                            result.key.level == level && result.key.coord == coord
                        })
                        .unwrap();
                    let TileLoadOutcome::Decoded(DecodedTile::Cpu(tile)) = result.outcome else {
                        panic!("CPU decode failed")
                    };
                    assert_eq!(
                        (tile.width, tile.height),
                        (oracle[index].width, oracle[index].height)
                    );
                    assert_eq!(tile.rgba, oracle[index].rgba);
                }
                println!("{{\"workload\":\"loader-warm\",\"tiles\":64,\"workers\":{workers},\"batch\":{batch},\"jp2k_threads\":\"{threads}\",\"sample\":{sample},\"ms\":{}}}", elapsed.as_secs_f64()*1000.0);
            }
        }
    }
}
