use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use dicom_viewer_core::{LevelIndex, TileCoord, ViewerStudy};
use rayon::prelude::*;

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("usage: tile_probe <wsi-file-or-dicom-folder>");
            std::process::exit(2);
        });

    let start = Instant::now();
    let study = Arc::new(ViewerStudy::open_path(&path).unwrap_or_else(|err| {
        eprintln!("open failed: {err}");
        std::process::exit(1);
    }));
    println!("open_ms={:.3}", start.elapsed().as_secs_f64() * 1000.0);

    let summary = study.summary();
    println!(
        "format={} tile_decode_backend={} files={} instances={} levels={}",
        summary.format_label,
        summary.tile_decode_backend,
        summary.file_count,
        summary.dicom_instance_count,
        summary.levels.len()
    );

    for level in &summary.levels {
        println!(
            "level={} size={}x{} downsample={:.3} layout={:?}",
            level.index, level.width, level.height, level.downsample, level.tile_layout
        );
    }

    for level in &summary.levels {
        let Some((cols, rows)) = level.tile_layout.grid_size() else {
            continue;
        };
        let samples = sample_tiles(cols, rows);
        let mut total_ms = 0.0;
        let mut max_ms = 0.0;
        let mut count = 0usize;
        for coord in samples {
            let start = Instant::now();
            let tile = study
                .read_tile_rgba(level.index, coord)
                .unwrap_or_else(|err| {
                    eprintln!(
                        "read failed: level={} col={} row={}: {err}",
                        level.index,
                        coord.col(),
                        coord.row()
                    );
                    std::process::exit(1);
                });
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            total_ms += elapsed_ms;
            max_ms = elapsed_ms.max(max_ms);
            count += 1;
            println!(
                "tile level={} col={} row={} size={}x{} rgba={} read_ms={:.3}",
                level.index,
                coord.col(),
                coord.row(),
                tile.width,
                tile.height,
                tile.rgba.len(),
                elapsed_ms
            );
        }
        if count > 0 {
            println!(
                "level_summary level={} samples={} avg_ms={:.3} max_ms={:.3}",
                level.index,
                count,
                total_ms / count as f64,
                max_ms
            );
        }
    }

    for level in summary.levels.iter().take(5) {
        let Some((cols, rows)) = level.tile_layout.grid_size() else {
            continue;
        };
        let start_col = (cols / 2).saturating_sub(2);
        let start_row = (rows / 2).saturating_sub(2);
        let block = (start_row..(start_row + 4).min(rows))
            .flat_map(|row| (start_col..(start_col + 4).min(cols)).map(move |col| (col, row)))
            .map(|(col, row)| TileCoord::new(col, row))
            .collect::<Vec<_>>();
        measure_block_sequential(&study, level.index, &block);
        measure_block_parallel(&study, level.index, &block);
        measure_block_batched(&study, level.index, &block);
    }
}

fn sample_tiles(cols: u64, rows: u64) -> Vec<TileCoord> {
    let mut samples = vec![
        TileCoord::new(0, 0),
        TileCoord::new(cols / 2, rows / 2),
        TileCoord::new(cols.saturating_sub(1), rows.saturating_sub(1)),
    ];
    samples.sort_unstable();
    samples.dedup();
    samples
}

fn measure_block_sequential(study: &ViewerStudy, level: LevelIndex, block: &[TileCoord]) {
    let start = Instant::now();
    for &coord in block {
        let _ = study.read_tile_rgba(level, coord).unwrap_or_else(|err| {
            eprintln!(
                "sequential read failed: level={level} col={} row={}: {err}",
                coord.col(),
                coord.row()
            );
            std::process::exit(1);
        });
    }
    println!(
        "block_sequential level={} tiles={} total_ms={:.3}",
        level,
        block.len(),
        start.elapsed().as_secs_f64() * 1000.0
    );
}

fn measure_block_parallel(study: &Arc<ViewerStudy>, level: LevelIndex, block: &[TileCoord]) {
    let start = Instant::now();
    block.par_iter().for_each(|&coord| {
        let _ = study.read_tile_rgba(level, coord).unwrap_or_else(|err| {
            eprintln!(
                "parallel read failed: level={level} col={} row={}: {err}",
                coord.col(),
                coord.row()
            );
            std::process::exit(1);
        });
    });
    println!(
        "block_parallel level={} tiles={} total_ms={:.3}",
        level,
        block.len(),
        start.elapsed().as_secs_f64() * 1000.0
    );
}

fn measure_block_batched(study: &ViewerStudy, level: LevelIndex, block: &[TileCoord]) {
    let start = Instant::now();
    let requests = block
        .iter()
        .map(|&coord| (level, coord))
        .collect::<Vec<_>>();
    let tiles = study.read_tiles_rgba(&requests).unwrap_or_else(|err| {
        eprintln!("batched read failed: level={level}: {err}");
        std::process::exit(1);
    });
    println!(
        "block_batched level={} tiles={} total_ms={:.3}",
        level,
        tiles.len(),
        start.elapsed().as_secs_f64() * 1000.0
    );
}
