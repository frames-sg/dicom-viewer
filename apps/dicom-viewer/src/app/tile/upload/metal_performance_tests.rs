use super::*;

#[test]
#[ignore = "local Metal routing diagnostic; requires DICOM_VIEWER_WSI_FIXTURE"]
fn metal_source_route_diagnostics() {
    let path = std::env::var_os("DICOM_VIEWER_WSI_FIXTURE").expect("trusted local fixture");
    let state = render_state().expect("a real renderer is required");
    let uploader = WgpuTileUploader::new(state);
    let options = uploader.viewer_open_options().expect("same-device options");
    let study = dicom_viewer_core::ViewerStudy::open_path_with_options(path, options).unwrap();
    eprintln!(
        "backend={:?} color_status={:?} color_mode={:?} warnings={:?}",
        study.summary().tile_decode_backend,
        study.summary().color_management.status,
        study.summary().color_management.applied_mode,
        study.summary().warnings
    );
}

#[test]
#[ignore = "release Metal import/conversion/registration characterization; requires a Metal adapter"]
fn metal_upload_release_characterization() {
    if cfg!(debug_assertions) {
        panic!("run with --release");
    }
    let state = render_state().expect("a real renderer is required");
    let device = state.device.clone();
    let queue = state.queue.clone();
    let renderer = Arc::clone(&state.renderer);
    let mut uploader = WgpuTileUploader::new(state);
    let lut = identity_lut("metal-performance-identity");
    for edge in [64u32, 256, 1024] {
        for batch in [1usize, 8] {
            let rgb: Vec<u8> = (0..edge as usize * edge as usize * 3)
                .map(|i| (i % 251) as u8)
                .collect();
            let expected: Vec<u8> = rgb
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect();
            let images: Vec<_> = (0..batch)
                .map(|_| {
                    uploader
                        .metal_bridge
                        .as_ref()
                        .expect("Metal adapter")
                        .resident_rgb8_test_fixture(&rgb, 0, (edge, edge), edge as usize * 3)
                        .unwrap()
                })
                .collect();
            for with_lut in [false, true] {
                for sample in 0..15 {
                    let started = Instant::now();
                    let prepared: Vec<_> = images
                        .iter()
                        .map(|image| {
                            uploader
                                .prepare_metal_image(image, with_lut.then_some(&lut))
                                .unwrap()
                        })
                        .collect();
                    let encode_ms = started.elapsed().as_secs_f64() * 1000.0;
                    let started_submit = Instant::now();
                    queue.submit([uploader
                        .encode_metal_conversions(prepared.iter())
                        .expect("Metal conversion encoder")]);
                    let uploads: Vec<_> = prepared
                        .into_iter()
                        .map(|prepared| uploader.register(prepared.texture))
                        .collect();
                    let submit_register_ms = started_submit.elapsed().as_secs_f64() * 1000.0;
                    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                    let completed_ms = started.elapsed().as_secs_f64() * 1000.0;
                    let ids: Vec<_> = uploads.iter().map(RegisteredTileTexture::id).collect();
                    for tile in &uploads {
                        assert!(renderer.read().texture(&tile.id()).is_some());
                        let actual =
                            super::tests::read_texture(&device, &queue, tile.texture(), edge, edge);
                        assert_eq!(actual.len(), expected.len());
                        for (index, (&actual, &expected)) in
                            actual.iter().zip(&expected).enumerate()
                        {
                            let tolerance = u8::from(with_lut && index % 4 != 3);
                            assert!(
                                actual.abs_diff(expected) <= tolerance,
                                "GPU output differs at byte {index}: {actual} vs {expected}"
                            );
                        }
                    }
                    drop(uploads);
                    assert!(ids.iter().all(|id| renderer.read().texture(id).is_none()));
                    println!("{{\"workload\":\"metal-upload\",\"edge\":{edge},\"batch\":{batch},\"lut\":{with_lut},\"sample\":{sample},\"encode_ms\":{encode_ms},\"submit_register_ms\":{submit_register_ms},\"completed_ms\":{completed_ms}}}");
                }
            }
        }
    }
}

#[test]
#[ignore = "release GPU timestamp comparison; requires Metal timestamp queries"]
fn metal_pass_gpu_timestamp_characterization() {
    // Debug is a small API/parity smoke check; comparative timings use release.
    let edges: &[u32] = if cfg!(debug_assertions) {
        &[64]
    } else {
        &[256, 1024]
    };
    let samples = if cfg!(debug_assertions) { 1 } else { 15 };
    let state = render_state_with_features(wgpu::Features::TIMESTAMP_QUERY)
        .expect("Metal timestamp queries are required");
    let device = state.device.clone();
    let queue = state.queue.clone();
    let mut uploader = WgpuTileUploader::new(state);
    let lut = identity_lut("timestamp-identity");
    for &edge in edges {
        let rgb: Vec<u8> = (0..edge as usize * edge as usize * 3)
            .map(|i| (i % 251) as u8)
            .collect();
        let expected: Vec<u8> = rgb
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
        let images: Vec<_> = (0..8)
            .map(|_| {
                uploader
                    .metal_bridge
                    .as_ref()
                    .unwrap()
                    .resident_rgb8_test_fixture(&rgb, 0, (edge, edge), edge as usize * 3)
                    .unwrap()
            })
            .collect();
        for with_lut in [false, true] {
            let prepared: Vec<_> = images
                .iter()
                .map(|image| {
                    uploader
                        .prepare_metal_image(image, with_lut.then_some(&lut))
                        .unwrap()
                })
                .collect();
            for sample in 0..samples {
                for batched in if sample % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                } {
                    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
                        label: Some("Metal conversion timestamps"),
                        ty: wgpu::QueryType::Timestamp,
                        count: 2,
                    });
                    let commands = timestamped_passes(&uploader, &prepared, &queries, batched);
                    let gpu_ms = resolve_gpu_time(&device, &queue, &queries, commands);
                    for conversion in &prepared {
                        let actual = super::tests::read_texture(
                            &device,
                            &queue,
                            &conversion.texture.texture,
                            edge,
                            edge,
                        );
                        assert_eq!(actual.len(), expected.len());
                        assert!(actual
                            .iter()
                            .zip(&expected)
                            .enumerate()
                            .all(|(i, (actual, expected))| actual.abs_diff(*expected)
                                <= u8::from(with_lut && i % 4 != 3)));
                    }
                    println!("{{\"workload\":\"metal-pass-gpu\",\"edge\":{edge},\"batch\":8,\"lut\":{with_lut},\"batched\":{batched},\"sample\":{sample},\"gpu_ms\":{gpu_ms}}}");
                }
            }
        }
    }
}

// Isolate pass grouping with identical prepared resources and shader. Timestamp
// only the first beginning and last ending, so both strategies use two samples.
fn timestamped_passes(
    uploader: &WgpuTileUploader,
    prepared: &[PreparedMetalConversion],
    queries: &wgpu::QuerySet,
    batched: bool,
) -> wgpu::CommandBuffer {
    let device = &uploader.context.state.device;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Metal pass grouping comparison"),
    });
    let chunks = prepared.chunks(if batched { prepared.len() } else { 1 });
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Metal pass grouping comparison"),
            timestamp_writes: (index == 0 || index + 1 == count).then_some(
                wgpu::ComputePassTimestampWrites {
                    query_set: queries,
                    beginning_of_pass_write_index: (index == 0).then_some(0),
                    end_of_pass_write_index: (index + 1 == count).then_some(1),
                },
            ),
        });
        pass.set_pipeline(&uploader.conversion_pipeline);
        for conversion in chunk {
            pass.set_bind_group(0, &conversion.bind_group, &[]);
            pass.dispatch_workgroups(
                conversion.texture.width.div_ceil(8),
                conversion.texture.height.div_ceil(8),
                1,
            );
        }
    }
    encoder.finish()
}

fn resolve_gpu_time(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    queries: &wgpu::QuerySet,
    commands: wgpu::CommandBuffer,
) -> f64 {
    // Keep resolution after completion and use a fresh query set per sample so
    // an earlier sample cannot be mistaken for this conversion's timestamps.
    let started = Instant::now();
    queue.submit([commands]);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let completed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let resolved = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("resolved Metal timestamps"),
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let mapped = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Metal timestamp readback"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.resolve_query_set(queries, 0..2, &resolved, 0);
    encoder.copy_buffer_to_buffer(&resolved, 0, &mapped, 0, 16);
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    mapped
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receiver.recv().unwrap().unwrap();
    let bytes = mapped.slice(..).get_mapped_range();
    let start = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let end = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    drop(bytes);
    mapped.unmap();
    assert!(
        start > 0 && end > start,
        "invalid GPU timestamps: {start}..{end}"
    );
    let gpu_ms = (end - start) as f64 * f64::from(queue.get_timestamp_period()) / 1_000_000.0;
    assert!(
        gpu_ms <= completed_ms,
        "GPU interval {gpu_ms} ms exceeds host completion {completed_ms} ms"
    );
    gpu_ms
}

fn identity_lut(label: &str) -> ColorLut3d {
    let mut rgba = Vec::with_capacity(65 * 65 * 65 * 4);
    for blue in 0..65 {
        for green in 0..65 {
            for red in 0..65 {
                rgba.extend_from_slice(
                    &[red, green, blue].map(|v| (v as f32 * 255.0 / 64.0).round() as u8),
                );
                rgba.push(255);
            }
        }
    }
    ColorLut3d::from_rgba8(65, rgba, label).unwrap()
}
