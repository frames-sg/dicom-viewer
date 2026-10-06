use super::*;

#[test]
fn cpu_color_upload_preserves_alpha_budget_and_memory_accounting() {
    let Some(state) = render_state() else { return };
    let device = state.device.clone();
    let queue = state.queue.clone();
    let mut uploader = WgpuTileUploader::new(state);
    let mut values = Vec::new();
    for b in [0u8, 255] {
        for g in [0u8, 255] {
            for r in [0u8, 255] {
                values.extend_from_slice(&[255 - r, 255 - g, 255 - b, 255]);
            }
        }
    }
    let lut = Arc::new(ColorLut3d::from_rgba8(2, values, "inverse CPU color").unwrap());
    let first = DecodedTile::CpuWithColorLut {
        tile: RgbaTile {
            width: 2,
            height: 1,
            rgba: vec![10, 20, 30, 17, 128, 64, 1, 0],
        },
        color_lut: Arc::clone(&lut),
    };
    assert!(first.is_cpu());
    assert_eq!(
        first.memory_cost().unwrap(),
        TileFootprint::for_cpu_rgba(2, 1, 8).unwrap()
    );
    let second_rgba = vec![20, 30, 40, 99];
    let second_pointer = second_rgba.as_ptr();
    let second = DecodedTile::CpuWithColorLut {
        tile: RgbaTile {
            width: 1,
            height: 1,
            rgba: second_rgba,
        },
        color_lut: Arc::clone(&lut),
    };
    let mut outcomes = uploader
        .upload_batch_budgeted(vec![first, second], Duration::ZERO)
        .into_iter();
    let BudgetedUploadOutcome::Ready(first) = outcomes.next().unwrap() else {
        panic!("first CPU color upload must proceed")
    };
    let BudgetedUploadOutcome::Deferred(DecodedTile::CpuWithColorLut { tile, color_lut }) =
        outcomes.next().unwrap()
    else {
        panic!("second CPU color upload must be deferred")
    };
    assert_eq!(tile.rgba.as_ptr(), second_pointer);
    assert!(Arc::ptr_eq(&color_lut, &lut));
    assert_eq!(uploader.submission_count(), 1);
    assert_eq!(
        super::tests::read_texture(&device, &queue, first.texture(), 2, 1),
        vec![245, 235, 225, 17, 127, 191, 254, 0]
    );
    let second = uploader
        .upload_batch(vec![DecodedTile::CpuWithColorLut { tile, color_lut }])
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(
        super::tests::read_texture(&device, &queue, second.texture(), 1, 1),
        vec![235, 225, 215, 99]
    );
}

#[test]
#[ignore = "release CPU versus Metal color pipeline; requires DICOM_VIEWER_WSI_FIXTURE"]
fn local_color_pipeline_release_characterization() {
    use dicom_viewer_core::{ReadControl, RenderTile, TileCoord, ViewerStudy};
    if cfg!(debug_assertions) {
        panic!("run with --release");
    }
    let path =
        std::env::var_os("DICOM_VIEWER_WSI_FIXTURE").expect("trusted profiled DICOM fixture");
    let state = render_state().expect("Metal renderer required");
    let device = state.device.clone();
    let queue = state.queue.clone();
    let mut uploader = WgpuTileUploader::new(state);
    // Separate source caches keep CPU reference reads from populating the
    // renderer study's decoded-frame cache and changing its output route.
    let started = Instant::now();
    let cpu = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    let cpu_open_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let metal = ViewerStudy::open_path_with_options(&path, uploader.viewer_open_options().unwrap())
        .unwrap();
    eprintln!(
        "cpu_open_ms={cpu_open_ms} metal_open_ms={} mode={:?}",
        started.elapsed().as_secs_f64() * 1000.0,
        metal.summary().color_management.applied_mode
    );
    assert_eq!(
        metal.summary().tile_decode_backend,
        dicom_viewer_core::TileDecodeBackend::Cpu
    );
    assert_eq!(
        metal.summary().color_management.applied_mode,
        dicom_viewer_core::ColorManagementMode::MetalExactLut
    );
    for level in &metal.summary().levels {
        let (cols, rows) = level.tile_layout.grid_size().unwrap();
        assert!(
            cols >= 6 && rows >= 4,
            "fixture must have eight full central tiles per level"
        );
        let mut requests = Vec::new();
        for row in rows / 2 - 1..rows / 2 + 1 {
            for col in cols / 2 - 2..cols / 2 + 2 {
                requests.push((level.index, TileCoord::new(col, row)));
            }
        }
        let expected = cpu
            .read_tiles_rgba_controlled(&requests, &ReadControl::default())
            .unwrap();
        for sample in 0..15 {
            for use_metal in if sample % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let source = if use_metal { &metal } else { &cpu };
                let started = Instant::now();
                let tiles = source
                    .read_tiles_for_render_controlled(&requests, &ReadControl::default())
                    .unwrap();
                let color_count = tiles
                    .iter()
                    .filter(|tile| matches!(tile, RenderTile::CpuWithColorLut { .. }))
                    .count();
                assert_eq!(color_count, if use_metal { 8 } else { 0 });
                let decoded = tiles
                    .into_iter()
                    .map(DecodedTile::from_render_tile)
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let uploaded = uploader
                    .upload_batch(decoded)
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                // Flush CPU queue writes too, as the following viewer paint
                // would. Both routes finish their color conversion/upload.
                queue.submit([]);
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                let completed_ms = started.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(uploaded.len(), expected.len());
                let mut max_delta = 0;
                for (tile, expected) in uploaded.iter().zip(&expected) {
                    let actual = super::tests::read_texture(
                        &device,
                        &queue,
                        tile.texture(),
                        expected.width,
                        expected.height,
                    );
                    assert_eq!(actual.len(), expected.rgba.len());
                    for (&a, &b) in actual.iter().zip(&expected.rgba) {
                        max_delta = max_delta.max(a.abs_diff(b));
                    }
                }
                assert_eq!(max_delta, 0, "both color routes must match bit-for-bit");
                println!("{{\"workload\":\"color-pipeline\",\"level\":{},\"batch\":8,\"metal\":{use_metal},\"sample\":{sample},\"completed_ms\":{completed_ms},\"max_delta\":{max_delta}}}", level.index.get());
            }
        }
    }
}

#[test]
#[ignore = "exhaustive exact-LUT GPU readback; requires a real Metal adapter"]
fn exact_color_lut_gpu_readback_covers_every_rgb8_input() {
    let state = render_state().expect("Metal renderer required");
    let device = state.device.clone();
    let queue = state.queue.clone();
    let renderer = Arc::clone(&state.renderer);
    let mut uploader = WgpuTileUploader::new(state);
    let mut rgba = Vec::with_capacity(256 * 256 * 256 * 4);
    for b in 0..=255u8 {
        for g in 0..=255u8 {
            for r in 0..=255u8 {
                // Discontinuous values expose accidental filtering and axis
                // swaps that an identity or linear table could conceal.
                rgba.extend_from_slice(&[
                    r.wrapping_mul(73) ^ g,
                    g.wrapping_mul(151) ^ b,
                    b.wrapping_mul(199) ^ r,
                    255,
                ]);
            }
        }
    }
    let lut = ColorLut3d::from_rgba8(256, rgba, "exact lookup regression").unwrap();
    for b in 0..=255u8 {
        let mut rgb = Vec::with_capacity(256 * 256 * 3);
        for g in 0..=255u8 {
            for r in 0..=255u8 {
                rgb.extend_from_slice(&[r, g, b]);
            }
        }
        let image = uploader
            .metal_bridge
            .as_ref()
            .unwrap()
            .resident_rgb8_test_fixture(&rgb, 0, (256, 256), 768)
            .unwrap();
        for from_cpu in [false, true] {
            let prepared = if from_cpu {
                let rgba = rgb
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2], 255])
                    .collect();
                uploader
                    .prepare_cpu_color_texture(
                        RgbaTile {
                            width: 256,
                            height: 256,
                            rgba,
                        },
                        &lut,
                    )
                    .unwrap()
            } else {
                uploader.prepare_metal_image(&image, Some(&lut)).unwrap()
            };
            // The prepared binding must own the 64 MiB texture through eviction.
            if b == 0 {
                uploader.clear_study_resources();
            }
            queue.submit([uploader
                .encode_metal_conversions(std::iter::once(&prepared))
                .unwrap()]);
            let uploaded = uploader.register(prepared.texture);
            let actual = super::tests::read_texture(&device, &queue, uploaded.texture(), 256, 256);
            let start = usize::from(b) * 256 * 256 * 4;
            assert_eq!(
                actual,
                lut.rgba()[start..start + 256 * 256 * 4],
                "blue plane {b}, CPU input {from_cpu}"
            );
            let id = uploaded.id();
            drop(uploaded);
            assert!(renderer.read().texture(&id).is_none());
        }
    }
    eprintln!("exact GPU lookup: all 16,777,216 RGB inputs through both source layouts, zero differing RGBA bytes");
}

#[test]
#[ignore = "exact-color DICOM regression; requires DICOM_VIEWER_WSI_FIXTURE"]
fn local_exact_color_profile_keeps_metal_pixels() {
    use dicom_viewer_core::{
        ColorManagementMode, ReadControl, RenderTile, TileCoord, TileDecodeBackend, ViewerStudy,
    };
    let path =
        std::env::var_os("DICOM_VIEWER_WSI_FIXTURE").expect("trusted profiled DICOM fixture");
    let state = render_state().expect("Metal renderer required");
    let device = state.device.clone();
    let queue = state.queue.clone();
    let mut uploader = WgpuTileUploader::new(state);
    let study =
        ViewerStudy::open_path_with_options(path, uploader.viewer_open_options().unwrap()).unwrap();
    assert_eq!(study.summary().tile_decode_backend, TileDecodeBackend::Cpu);
    assert_eq!(
        study.summary().color_management.applied_mode,
        ColorManagementMode::MetalExactLut
    );
    let mut requests = Vec::new();
    for level in &study.summary().levels {
        let (cols, rows) = level.tile_layout.grid_size().unwrap();
        for (col, row) in [(0, 0), (cols / 2, rows / 2), (cols - 1, rows - 1)] {
            requests.push((level.index, TileCoord::new(col, row)));
        }
    }
    let tiles = study
        .read_tiles_for_render_controlled(&requests, &ReadControl::default())
        .unwrap();
    assert_eq!(tiles.len(), requests.len());
    assert!(
        tiles
            .iter()
            .all(|tile| matches!(tile, RenderTile::CpuWithColorLut { .. })),
        "both full and cropped tiles must defer color to Metal"
    );
    let decoded = tiles
        .into_iter()
        .map(DecodedTile::from_render_tile)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let actual = uploader.upload_batch(decoded);
    let expected = study
        .read_tiles_rgba_controlled(&requests, &ReadControl::default())
        .unwrap();
    for (index, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
        let actual = actual.unwrap();
        assert_eq!(actual.dimensions(), (expected.width, expected.height));
        let rgba = super::tests::read_texture(
            &device,
            &queue,
            actual.texture(),
            expected.width,
            expected.height,
        );
        assert_eq!(rgba.len(), expected.rgba.len());
        let error = rgba
            .iter()
            .zip(&expected.rgba)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert_eq!(
            error, 0,
            "tile {index}: GPU color differs by {error} code values"
        );
    }
    eprintln!(
        "{} CPU-decoded DICOM tiles used Metal color conversion and matched CPU bit-for-bit",
        requests.len()
    );
}
