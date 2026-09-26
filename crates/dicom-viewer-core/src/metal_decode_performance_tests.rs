//! Source-only diagnostic: bypass ICC routing without changing viewer policy.

use super::*;
use std::time::Instant;

#[test]
#[ignore = "release Metal decode investigation; requires DICOM_VIEWER_WSI_FIXTURE"]
fn local_native_metal_decode_characterization() {
    if cfg!(debug_assertions) {
        panic!("run with --release");
    }
    let path = std::env::var_os("DICOM_VIEWER_WSI_FIXTURE").expect("trusted local fixture");
    let study = ViewerStudy::open_path_with_options(path, ViewerOpenOptions::cpu_only()).unwrap();
    let device = j2k_metal_support::system_default_device().unwrap();
    let output = TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(
        wsi_rs::output::metal::MetalBackendSessions::new(device),
    )
    .without_adaptive_decode_route();
    let samples = std::env::var("DICOM_VIEWER_DECODE_SAMPLES")
        .map(|value| value.parse::<usize>().expect("positive sample count"))
        .unwrap_or(3);
    assert!(samples > 0);
    for level in &study.summary.levels {
        let LevelTileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } = level.tile_layout
        else {
            panic!("regular fixture required");
        };
        assert!(tiles_across >= 9 && tiles_down >= 3);
        let requests = (0..8)
            .map(|offset| {
                build_tile_request(
                    study.selected_view,
                    level.index,
                    TileCoord::new(tiles_across / 2 - 4 + offset, tiles_down / 2),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let raw = study.slide.read_raw_compressed_tile(&requests[0]).unwrap();
        let image =
            j2k_native::Image::new(raw.data(), &j2k_native::DecodeSettings::default()).unwrap();
        let plan = image.build_direct_color_plan_with_context(&mut Default::default());
        eprintln!(
            "native_plan level={} result={:?}",
            level.index.get(),
            plan.as_ref()
                .map(|plan| (plan.dimensions, plan.transform, plan.mct))
        );
        if let Some(directory) = std::env::var_os("DICOM_VIEWER_DECODE_EXPORT") {
            for (index, request) in requests.iter().enumerate() {
                let raw = study.slide.read_raw_compressed_tile(request).unwrap();
                std::fs::write(
                    Path::new(&directory)
                        .join(format!("level{}-tile{index}.j2k", level.index.get())),
                    raw.data(),
                )
                .unwrap();
            }
        }
        for sample in 0..samples {
            let started = Instant::now();
            let tiles = study.slide.read_tiles(&requests, output.clone()).unwrap();
            let elapsed = started.elapsed();
            assert_eq!(tiles.len(), requests.len());
            for tile in &tiles {
                let TilePixels::Device(wsi_rs::DeviceTile::Metal(tile)) = tile else {
                    panic!("required native Metal output returned CPU pixels");
                };
                assert_eq!((tile.width, tile.height), (tile_width, tile_height));
                tile.validated_resident_image().unwrap();
            }
            eprintln!(
                "native_decode level={} sample={sample} count={} wall_ms={:.3}",
                level.index.get(),
                tiles.len(),
                elapsed.as_secs_f64() * 1000.0
            );
        }
    }
}
