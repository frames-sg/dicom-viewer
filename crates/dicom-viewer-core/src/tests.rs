use std::path::PathBuf;

use super::*;
use dicom_core::value::PrimitiveValue;
#[cfg(any(target_os = "macos", feature = "cuda"))]
use dicom_core::value::{fragments::Fragments, PixelFragmentSequence, Value};
use dicom_core::{DataElement, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use crate::inspection::{
    build_fact_warnings, candidate_paths_with_limit, canonical_canvas_dimensions, inspect_input,
    open_metadata_object, select_primary_view, summarize_renderable_levels,
    MAX_METADATA_ELEMENT_BYTES, MAX_METADATA_SEQUENCE_DEPTH, MAX_METADATA_VALUE_BYTES,
};

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

#[test]
fn viewer_error_reports_typed_cancellation() {
    assert!(ViewerError::Wsi(wsi_rs::WsiError::Cancelled).is_cancelled());
    assert!(!ViewerError::Wsi(wsi_rs::WsiError::BackendContract {
        context: "test",
        expected: 1,
        actual: 0,
    })
    .is_cancelled());
    assert!(!ViewerError::InvalidInput("not cancelled".into()).is_cancelled());
}

#[test]
fn jp2k_cpu_decode_budget_leaves_one_available_processor_for_the_viewer() {
    assert_eq!(jp2k_cpu_decode_thread_budget(1, None).get(), 1);
    assert_eq!(jp2k_cpu_decode_thread_budget(2, None).get(), 1);
    assert_eq!(jp2k_cpu_decode_thread_budget(12, None).get(), 11);
    assert_eq!(jp2k_cpu_decode_thread_budget(12, Some("2")).get(), 2);
    assert_eq!(jp2k_cpu_decode_thread_budget(12, Some("99")).get(), 12);
    assert_eq!(jp2k_cpu_decode_thread_budget(12, Some("invalid")).get(), 11);
}

#[test]
fn opened_study_applies_the_jp2k_cpu_decode_budget_to_wsi_rs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 16)).unwrap();

    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();

    let expected = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .saturating_sub(1)
        .max(1);
    assert_eq!(
        study
            .slide
            .decode_execution_options()
            .jp2k_cpu_threads()
            .map(std::num::NonZeroUsize::get),
        Some(expected)
    );
}

#[test]
fn controlled_viewer_read_honors_pre_cancelled_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 16)).unwrap();
    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    let token = wsi_rs::ReadCancellationToken::new();
    token.cancel();

    let error = study
        .read_tiles_rgba_controlled(
            &[(LevelIndex::from_u32(0), TileCoord::new(0, 0))],
            &wsi_rs::ReadControl::new(token),
        )
        .unwrap_err();

    assert!(matches!(
        error,
        ViewerError::Wsi(wsi_rs::WsiError::Cancelled)
    ));
}

#[test]
fn controlled_level_preparation_honors_pre_cancelled_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 16)).unwrap();
    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    let token = wsi_rs::ReadCancellationToken::new();
    token.cancel();

    let error = study
        .prepare_level_controlled(LevelIndex::from_u32(0), &wsi_rs::ReadControl::new(token))
        .unwrap_err();

    assert!(matches!(
        error,
        ViewerError::Wsi(wsi_rs::WsiError::Cancelled)
    ));
}

#[test]
fn empty_batch_policy_is_consistent_across_public_batch_apis() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 16)).unwrap();
    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    let active_control = wsi_rs::ReadControl::new(wsi_rs::ReadCancellationToken::new());

    assert!(study.read_tiles_rgba(&[]).unwrap().is_empty());
    assert!(study.read_tiles_for_render(&[]).unwrap().is_empty());
    assert!(study
        .read_tiles_rgba_controlled(&[], &active_control)
        .unwrap()
        .is_empty());
    assert!(study
        .read_tiles_for_render_controlled(&[], &active_control)
        .unwrap()
        .is_empty());

    let cancelled_token = wsi_rs::ReadCancellationToken::new();
    cancelled_token.cancel();
    let cancelled_control = wsi_rs::ReadControl::new(cancelled_token);
    assert!(study
        .read_tiles_rgba_controlled(&[], &cancelled_control)
        .unwrap_err()
        .is_cancelled());
    assert!(study
        .read_tiles_for_render_controlled(&[], &cancelled_control)
        .unwrap_err()
        .is_cancelled());
}

#[test]
fn public_batch_apis_share_validation_policy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 16)).unwrap();
    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    let control = wsi_rs::ReadControl::new(wsi_rs::ReadCancellationToken::new());

    for requests in [
        [(LevelIndex::from_u32(99), TileCoord::new(0, 0))],
        [(LevelIndex::from_u32(0), TileCoord::new(1, 0))],
    ] {
        let expected = study.read_tiles_rgba(&requests).unwrap_err().to_string();
        assert_eq!(
            study
                .read_tiles_rgba_controlled(&requests, &control)
                .unwrap_err()
                .to_string(),
            expected
        );
        assert_eq!(
            study
                .read_tiles_for_render(&requests)
                .unwrap_err()
                .to_string(),
            expected
        );
        assert_eq!(
            study
                .read_tiles_for_render_controlled(&requests, &control)
                .unwrap_err()
                .to_string(),
            expected
        );
    }
}

#[test]
fn controlled_and_uncontrolled_batches_share_cpu_conversion_policy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 12)).unwrap();
    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    let requests = [
        (LevelIndex::from_u32(0), TileCoord::new(0, 0)),
        (LevelIndex::from_u32(0), TileCoord::new(0, 0)),
    ];
    let control = wsi_rs::ReadControl::new(wsi_rs::ReadCancellationToken::new());

    let rgba = study.read_tiles_rgba(&requests).unwrap();
    let controlled_rgba = study
        .read_tiles_rgba_controlled(&requests, &control)
        .unwrap();
    assert_eq!(controlled_rgba.len(), rgba.len());
    for (actual, expected) in controlled_rgba.iter().zip(&rgba) {
        assert_eq!(
            (actual.width, actual.height),
            (expected.width, expected.height)
        );
        assert_eq!(actual.rgba, expected.rgba);
    }

    for tiles in [
        study.read_tiles_for_render(&requests).unwrap(),
        study
            .read_tiles_for_render_controlled(&requests, &control)
            .unwrap(),
    ] {
        assert_eq!(tiles.len(), rgba.len());
        for (actual, expected) in tiles.into_iter().zip(&rgba) {
            let RenderTile::Cpu(actual) = actual else {
                panic!("CPU-only viewer options returned a device tile");
            };
            assert_eq!(
                (actual.width, actual.height),
                (expected.width, expected.height)
            );
            assert_eq!(actual.rgba, expected.rgba);
        }
    }
}

#[test]
fn whole_level_batch_fallback_shares_control_and_conversion_policy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 12)).unwrap();
    let mut study =
        ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    study.summary.levels[0].tile_layout = LevelTileLayout::WholeLevel {
        width: 16,
        height: 12,
        virtual_tile_width: 16,
        virtual_tile_height: 12,
    };
    let requests = [(LevelIndex::from_u32(0), TileCoord::new(0, 0))];
    let control = wsi_rs::ReadControl::new(wsi_rs::ReadCancellationToken::new());

    let expected = study.read_tiles_rgba(&requests).unwrap().pop().unwrap();
    let actual = study
        .read_tiles_rgba_controlled(&requests, &control)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        (actual.width, actual.height),
        (expected.width, expected.height)
    );
    assert_eq!(actual.rgba, expected.rgba);

    for mut tiles in [
        study.read_tiles_for_render(&requests).unwrap(),
        study
            .read_tiles_for_render_controlled(&requests, &control)
            .unwrap(),
    ] {
        let RenderTile::Cpu(actual) = tiles.pop().unwrap() else {
            panic!("whole-level fallback returned a device tile");
        };
        assert_eq!(
            (actual.width, actual.height),
            (expected.width, expected.height)
        );
        assert_eq!(actual.rgba, expected.rgba);
    }
}

#[test]
fn extracts_synthetic_wsi_facts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.dcm");
    write_test_dicom(
        &path,
        "1.2.826.0.1.3680043.10.777.1",
        "1.2.826.0.1.3680043.10.777",
    );

    let inspection = inspect_input(&path).unwrap();
    assert_eq!(inspection.instances.len(), 1);
    let instance = &inspection.instances[0];
    assert_eq!(instance.rows, Some(2));
    assert_eq!(instance.columns, Some(2));
    assert_eq!(instance.number_of_frames, Some(1));
    assert_eq!(
        instance.dimension_organization_type.as_deref(),
        Some("TILED_FULL")
    );
    assert_eq!(instance.samples_per_pixel, Some(3));
    assert_eq!(instance.photometric_interpretation.as_deref(), Some("RGB"));
    assert_eq!(instance.bits_stored, Some(8));
    assert_eq!(
        instance.transfer_syntax_uid,
        uids::EXPLICIT_VR_LITTLE_ENDIAN
    );
}

#[test]
fn metadata_preflight_stops_before_pixel_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.dcm");
    write_test_dicom(
        &path,
        "1.2.826.0.1.3680043.10.777.1",
        "1.2.826.0.1.3680043.10.777",
    );

    let obj = open_metadata_object(&path).unwrap();
    assert!(obj.get(tags::PIXEL_DATA).is_none());
}

#[test]
fn metadata_preflight_rejects_an_oversized_declared_value_before_allocating_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oversized-metadata.dcm");
    write_test_dicom(
        &path,
        "1.2.826.0.1.3680043.10.777.2",
        "1.2.826.0.1.3680043.10.777",
    );
    let mut bytes = std::fs::read(&path).unwrap();
    let pixel_header = [0xE0, 0x7F, 0x10, 0x00, b'O', b'B', 0, 0];
    let pixel_offset = bytes
        .windows(pixel_header.len())
        .position(|candidate| candidate == pixel_header)
        .expect("test DICOM should contain explicit-VR Pixel Data");
    let mut hostile_header = vec![0x77, 0x77, 0x10, 0x00, b'O', b'B', 0, 0];
    hostile_header.extend_from_slice(&(MAX_METADATA_ELEMENT_BYTES + 1).to_le_bytes());
    bytes.splice(pixel_offset..pixel_offset, hostile_header);
    std::fs::write(&path, bytes).unwrap();

    let error = open_metadata_object(&path).unwrap_err();

    assert!(
        error.to_string().contains("metadata element value limit"),
        "unexpected error: {error}"
    );
}

#[test]
fn metadata_preflight_rejects_excessive_sequence_nesting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested-metadata.dcm");
    write_test_dicom(
        &path,
        "1.2.826.0.1.3680043.10.777.3",
        "1.2.826.0.1.3680043.10.777",
    );
    let mut bytes = std::fs::read(&path).unwrap();
    let pixel_header = [0xE0, 0x7F, 0x10, 0x00, b'O', b'B', 0, 0];
    let pixel_offset = bytes
        .windows(pixel_header.len())
        .position(|candidate| candidate == pixel_header)
        .expect("test DICOM should contain explicit-VR Pixel Data");
    let mut nested = Vec::new();
    for index in 0..=MAX_METADATA_SEQUENCE_DEPTH {
        nested.extend_from_slice(&[0x77, 0x77]);
        nested.extend_from_slice(&(0x1000_u16 + index as u16).to_le_bytes());
        nested.extend_from_slice(b"SQ");
        nested.extend_from_slice(&[0, 0]);
        nested.extend_from_slice(&u32::MAX.to_le_bytes());
        nested.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
        nested.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    bytes.splice(pixel_offset..pixel_offset, nested);
    std::fs::write(&path, bytes).unwrap();

    let error = open_metadata_object(&path).unwrap_err();

    assert!(
        error.to_string().contains("sequence nesting exceeds"),
        "unexpected error: {error}"
    );
}

#[test]
fn metadata_preflight_accepts_declared_values_and_nesting_at_the_limits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bounded-metadata.dcm");
    write_test_dicom(
        &path,
        "1.2.826.0.1.3680043.10.777.4",
        "1.2.826.0.1.3680043.10.777",
    );
    let mut bytes = std::fs::read(&path).unwrap();
    let pixel_header = [0xE0, 0x7F, 0x10, 0x00, b'O', b'B', 0, 0];
    let pixel_offset = bytes
        .windows(pixel_header.len())
        .position(|candidate| candidate == pixel_header)
        .expect("test DICOM should contain explicit-VR Pixel Data");
    let mut bounded = Vec::new();
    for index in 0..MAX_METADATA_SEQUENCE_DEPTH {
        bounded.extend_from_slice(&[0x77, 0x77]);
        bounded.extend_from_slice(&(0x1000_u16 + index as u16).to_le_bytes());
        bounded.extend_from_slice(b"SQ");
        bounded.extend_from_slice(&[0, 0]);
        bounded.extend_from_slice(&u32::MAX.to_le_bytes());
        bounded.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
        bounded.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    bounded.extend_from_slice(&[0x77, 0x77, 0x00, 0x20, b'O', b'B', 0, 0]);
    bounded.extend_from_slice(&MAX_METADATA_ELEMENT_BYTES.to_le_bytes());
    bounded.resize(bounded.len() + MAX_METADATA_ELEMENT_BYTES as usize, 0);
    for _ in 0..MAX_METADATA_SEQUENCE_DEPTH {
        bounded.extend_from_slice(&[0xFE, 0xFF, 0x0D, 0xE0, 0, 0, 0, 0]);
        bounded.extend_from_slice(&[0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0]);
    }
    bytes.splice(pixel_offset..pixel_offset, bounded);
    std::fs::write(&path, bytes).unwrap();

    let object = open_metadata_object(&path).unwrap();

    assert!(object.get(Tag(0x7777, 0x1000)).is_some());
    assert!(object.get(tags::PIXEL_DATA).is_none());
}

#[test]
fn metadata_preflight_rejects_values_over_the_cumulative_budget() {
    use std::io::{Seek, SeekFrom, Write};

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.dcm");
    let path = dir.path().join("cumulative-metadata.dcm");
    write_test_dicom(
        &source,
        "1.2.826.0.1.3680043.10.777.5",
        "1.2.826.0.1.3680043.10.777",
    );
    let bytes = std::fs::read(&source).unwrap();
    let pixel_header = [0xE0, 0x7F, 0x10, 0x00, b'O', b'B', 0, 0];
    let pixel_offset = bytes
        .windows(pixel_header.len())
        .position(|candidate| candidate == pixel_header)
        .expect("test DICOM should contain explicit-VR Pixel Data");
    let mut hostile = std::fs::File::create(&path).unwrap();
    hostile.write_all(&bytes[..pixel_offset]).unwrap();
    let element_count =
        MAX_METADATA_VALUE_BYTES.div_ceil(u64::from(MAX_METADATA_ELEMENT_BYTES)) + 1;
    for index in 0..element_count {
        hostile.write_all(&[0x77, 0x77]).unwrap();
        hostile
            .write_all(&(0x3000_u16 + index as u16).to_le_bytes())
            .unwrap();
        hostile.write_all(b"OB").unwrap();
        hostile.write_all(&[0, 0]).unwrap();
        hostile
            .write_all(&MAX_METADATA_ELEMENT_BYTES.to_le_bytes())
            .unwrap();
        hostile
            .seek(SeekFrom::Current(i64::from(MAX_METADATA_ELEMENT_BYTES)))
            .unwrap();
    }
    hostile.write_all(&bytes[pixel_offset..]).unwrap();
    hostile.sync_all().unwrap();

    let error = open_metadata_object(&path).unwrap_err();

    assert!(
        error.to_string().contains("cumulative value limit"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_non_dicom_input_without_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-dicom.dcm");
    std::fs::write(&path, b"not dicom").unwrap();

    let err = ViewerStudy::open_path(&path).unwrap_err();
    assert!(
        matches!(err, ViewerError::Wsi(_)),
        "unexpected error: {err:?}"
    );
}

#[test]
fn opens_wsi_rs_raw_jp2k_without_dicom_instances() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(32, 24)).unwrap();

    let study = ViewerStudy::open_path(&path).unwrap();
    let summary = study.summary();
    assert_eq!(summary.format_label, "Raw JPEG 2000 WSI");
    assert_eq!(summary.file_count, 1);
    assert_eq!(summary.dicom_instance_count, 0);
    assert_eq!(summary.levels.len(), 1);
    assert_eq!(study.selected_view.scene.get(), 0);
    assert_eq!(study.selected_view.series.get(), 0);
    assert_eq!(study.selected_view.plane, wsi_rs::PlaneIdx::default());

    let tile = study
        .read_tile_rgba(LevelIndex::from_u32(0), TileCoord::new(0, 0))
        .unwrap();
    assert_eq!((tile.width, tile.height), (32, 24));
    assert_eq!(tile.rgba.len(), 32 * 24 * 4);
}

#[test]
fn render_tile_api_preserves_cpu_output_and_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(32, 24)).unwrap();
    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    let requests = [
        (LevelIndex::from_u32(0), TileCoord::new(0, 0)),
        (LevelIndex::from_u32(0), TileCoord::new(0, 0)),
    ];

    let tiles = study.read_tiles_for_render(&requests).unwrap();

    assert_eq!(tiles.len(), requests.len());
    for tile in tiles {
        let RenderTile::Cpu(tile) = tile else {
            panic!("CPU-only viewer options returned a device tile");
        };
        assert_eq!((tile.width, tile.height), (32, 24));
        assert_eq!(tile.rgba.len(), 32 * 24 * 4);
    }
    assert_eq!(study.summary().tile_decode_backend, TileDecodeBackend::Cpu);
}

#[test]
fn existing_rgba_api_remains_cpu_resident_with_render_options() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 12)).unwrap();
    let study = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::auto()).unwrap();

    let tile = study
        .read_tile_rgba(LevelIndex::from_u32(0), TileCoord::new(0, 0))
        .unwrap();

    assert_eq!((tile.width, tile.height), (16, 12));
    assert_eq!(tile.rgba.len(), 16 * 12 * 4);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_metal_options_return_resident_tiles_for_synthetic_dicom_htj2k() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metal.dcm");
    write_test_htj2k_dicom(&path, htj2k_rgb8_fixture(2, 2));
    let study = ViewerStudy::open_path_with_options(
        &path,
        ViewerOpenOptions::auto().with_metal_device(device.clone()),
    )
    .unwrap();
    assert!(
        !study.render_tile_output.adaptive_decode_route_enabled(),
        "the viewer's explicit same-device DICOM Metal path must not spend the first batch probing CPU throughput"
    );
    let requests = (0..8)
        .map(|col| (LevelIndex::from_u32(0), TileCoord::new(col, 0)))
        .collect::<Vec<_>>();
    let raw_requests = requests
        .iter()
        .map(|&(level, coord)| build_tile_request(study.selected_view, level, coord).unwrap())
        .collect::<Vec<_>>();
    let required = study
        .slide
        .read_tiles(
            &raw_requests,
            wsi_rs::TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(
                wsi_rs::output::metal::MetalBackendSessions::new(device),
            ),
        )
        .unwrap();
    assert!(required
        .iter()
        .all(|tile| matches!(tile, wsi_rs::TilePixels::Device(_))));

    let tiles = study.read_tiles_for_render(&requests).unwrap();

    assert_eq!(tiles.len(), 8);
    for tile in tiles {
        let RenderTile::Metal(tile) = tile else {
            panic!("Metal-preferred synthetic DICOM HTJ2K batch returned host pixels");
        };
        assert_eq!((tile.width(), tile.height()), (2, 2));
        assert!(tile.byte_len() >= 12);
    }
}

#[cfg(all(feature = "cuda", not(target_os = "macos")))]
#[test]
fn cuda_viewer_download_matches_strict_cpu_for_synthetic_dicom_htj2k() {
    let require_cuda = std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_some();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cuda.dcm");
    write_test_htj2k_dicom(&path, htj2k_rgb8_fixture(2, 2));
    let cpu = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::cpu_only()).unwrap();
    let expected = cpu
        .read_tiles_rgba(&[(LevelIndex::from_u32(0), TileCoord::new(0, 0))])
        .unwrap()
        .remove(0);
    let cuda = ViewerStudy::open_path_with_options(&path, ViewerOpenOptions::auto()).unwrap();
    assert_eq!(cuda.summary().tile_decode_backend, TileDecodeBackend::Cuda);

    let actual =
        match cuda.read_tiles_for_render(&[(LevelIndex::from_u32(0), TileCoord::new(0, 0))]) {
            Ok(mut tiles) => tiles.remove(0),
            Err(error) if !require_cuda => {
                eprintln!("skipping CUDA viewer parity without required runtime: {error}");
                return;
            }
            Err(error) => panic!("required CUDA viewer decode/download failed: {error}"),
        };
    let RenderTile::Cpu(actual) = actual else {
        panic!("CUDA renderer boundary must return downloaded CPU pixels to wgpu");
    };

    assert_eq!(
        (actual.width, actual.height),
        (expected.width, expected.height)
    );
    assert_eq!(actual.rgba, expected.rgba);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_metal_options_keep_adaptive_routing_for_non_dicom_sources() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("adaptive.j2k");
    std::fs::write(&path, htj2k_rgb8_fixture(16, 12)).unwrap();

    let study = ViewerStudy::open_path_with_options(
        &path,
        ViewerOpenOptions::auto().with_metal_device(device),
    )
    .unwrap();

    assert!(
        study.render_tile_output.adaptive_decode_route_enabled(),
        "the DICOM-specific first-batch optimization must not disable adaptive routing for other formats"
    );
}

#[test]
fn primary_view_selection_warns_about_additional_views_and_planes() {
    use wsi_rs::{
        AxesShape, ChannelInfo, Dataset, DatasetId, Level, SampleType, Scene, Series, TileLayout,
    };

    let level = || {
        Level::new(
            (16, 16),
            1.0,
            TileLayout::Regular {
                tile_width: 16,
                tile_height: 16,
                tiles_across: 1,
                tiles_down: 1,
            },
        )
    };
    let series = |id: &str, axes| {
        Series::new(
            id,
            axes,
            vec![level()],
            SampleType::Uint8,
            vec![ChannelInfo::new()],
        )
    };
    let dataset = Dataset::new(
        DatasetId::new(7),
        vec![
            Scene::new(
                "first",
                vec![
                    series("primary", AxesShape::new(2, 3, 4)),
                    series("secondary", AxesShape::default()),
                ],
            ),
            Scene::new("second", vec![series("other", AxesShape::default())]),
        ],
    );

    let (selected, warnings) = select_primary_view(&dataset).unwrap();

    assert_eq!(selected.scene.get(), 0);
    assert_eq!(selected.series.get(), 0);
    assert_eq!(selected.plane, wsi_rs::PlaneIdx::default());
    assert!(warnings.iter().any(|warning| warning.contains("2 scenes")));
    assert!(warnings.iter().any(|warning| warning.contains("2 series")));
    assert!(warnings.iter().any(|warning| warning.contains("2/3/4")));
}

#[test]
fn primary_view_selection_rejects_unsupported_sample_type() {
    use wsi_rs::{
        AxesShape, ChannelInfo, Dataset, DatasetId, Level, SampleType, Scene, Series, TileLayout,
    };

    let dataset = Dataset::new(
        DatasetId::new(8),
        vec![Scene::new(
            "scene",
            vec![Series::new(
                "series",
                AxesShape::default(),
                vec![Level::new(
                    (1, 1),
                    1.0,
                    TileLayout::Regular {
                        tile_width: 1,
                        tile_height: 1,
                        tiles_across: 1,
                        tiles_down: 1,
                    },
                )],
                SampleType::Uint16,
                vec![ChannelInfo::new()],
            )],
        )],
    );

    let err = select_primary_view(&dataset).unwrap_err();

    assert!(
        matches!(&err, ViewerError::Unsupported(message) if message.contains("Uint8") && message.contains("Uint16")),
        "unexpected error: {err:?}"
    );
}

#[test]
fn tile_requests_use_the_selected_scene_series_and_plane() {
    let plane = wsi_rs::PlaneIdx::new(wsi_rs::PlaneSelection::new(2, 3, 4));
    let selected = crate::model::SelectedView {
        scene: wsi_rs::SceneId::new(5),
        series: wsi_rs::SeriesId::new(6),
        plane,
    };
    let level = LevelIndex::from_u32(7);
    let coord = TileCoord::new(8, 9);

    let source = build_tile_request(selected, level, coord).unwrap();
    let display = build_tile_view_request(selected, level, coord, 512, 256).unwrap();

    assert_eq!(source.scene.get(), 5);
    assert_eq!(source.series.get(), 6);
    assert_eq!(source.level.get(), 7);
    assert_eq!(source.plane, plane);
    assert_eq!((source.col, source.row), (8, 9));
    assert_eq!(display.scene.get(), 5);
    assert_eq!(display.series.get(), 6);
    assert_eq!(display.level.get(), 7);
    assert_eq!(display.plane, plane);
    assert_eq!((display.col, display.row), (8, 9));
    assert_eq!((display.tile_width, display.tile_height), (512, 256));
}

#[test]
fn renderable_levels_skip_known_irregular_layouts_without_renumbering() {
    use std::collections::HashMap;
    use wsi_rs::{AxesShape, ChannelInfo, Level, SampleType, Series, TileLayout};

    let series = Series::new(
        "series",
        AxesShape::default(),
        vec![
            Level::new(
                (512, 512),
                1.0,
                TileLayout::Irregular {
                    tile_advance: (256.0, 256.0),
                    extra_tiles: (0, 0, 0, 0),
                    tiles: HashMap::new(),
                },
            ),
            Level::new(
                (128, 128),
                4.0,
                TileLayout::Regular {
                    tile_width: 128,
                    tile_height: 128,
                    tiles_across: 1,
                    tiles_down: 1,
                },
            ),
        ],
        SampleType::Uint8,
        vec![ChannelInfo::new()],
    );

    let (levels, warnings) = summarize_renderable_levels(&series).unwrap();

    assert_eq!(levels.len(), 1);
    assert_eq!(levels[0].index, LevelIndex::from_u32(1));
    assert!(warnings
        .iter()
        .any(|warning| warning.contains("1 irregular-layout level")));
}

#[test]
fn canvas_dimensions_use_all_valid_source_levels_and_round_up() {
    use std::collections::HashMap;
    use wsi_rs::{AxesShape, ChannelInfo, Level, SampleType, Series, TileLayout};

    let series = Series::new(
        "series",
        AxesShape::default(),
        vec![
            Level::new(
                (1001, 777),
                1.0,
                TileLayout::Irregular {
                    tile_advance: (256.0, 256.0),
                    extra_tiles: (0, 0, 0, 0),
                    tiles: HashMap::new(),
                },
            ),
            Level::new(
                (250, 194),
                4.0,
                TileLayout::Regular {
                    tile_width: 128,
                    tile_height: 128,
                    tiles_across: 2,
                    tiles_down: 2,
                },
            ),
            Level::new(
                (323, 201),
                3.1,
                TileLayout::Regular {
                    tile_width: 128,
                    tile_height: 128,
                    tiles_across: 3,
                    tiles_down: 2,
                },
            ),
        ],
        SampleType::Uint8,
        vec![ChannelInfo::new()],
    );

    // The skipped leading irregular level still defines the height, while the
    // third level's non-integral extent rounds up to define the width.
    assert_eq!(canonical_canvas_dimensions(&series).unwrap(), (1002, 777));
}

#[test]
fn irregular_only_series_is_rejected_during_open_summary() {
    use std::collections::HashMap;
    use wsi_rs::{AxesShape, ChannelInfo, Level, SampleType, Series, TileLayout};

    let series = Series::new(
        "series",
        AxesShape::default(),
        vec![Level::new(
            (512, 512),
            1.0,
            TileLayout::Irregular {
                tile_advance: (256.0, 256.0),
                extra_tiles: (0, 0, 0, 0),
                tiles: HashMap::new(),
            },
        )],
        SampleType::Uint8,
        vec![ChannelInfo::new()],
    );

    let err = summarize_renderable_levels(&series).unwrap_err();

    assert!(
        matches!(&err, ViewerError::Unsupported(message) if message.contains("no regular or whole-level")),
        "unexpected error: {err:?}"
    );
}

#[test]
fn folder_scan_rejects_mixed_series_candidates() {
    let dir = tempfile::tempdir().unwrap();
    write_test_dicom(
        &dir.path().join("a.dcm"),
        "1.2.826.0.1.3680043.10.777.1",
        "1.2.826.0.1.3680043.10.777",
    );
    write_test_dicom(
        &dir.path().join("b.dcm"),
        "1.2.826.0.1.3680043.10.778.1",
        "1.2.826.0.1.3680043.10.778",
    );

    let err = inspect_input(dir.path()).unwrap_err();
    assert!(
        matches!(&err, ViewerError::InvalidInput(message) if message.contains("2 distinct DICOM series")),
        "mixed folder scan should fail before opening pixels: {err:?}"
    );
}

#[test]
fn non_dicom_single_file_skips_dicom_metadata_inspection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.j2k");
    std::fs::write(&path, b"not a DICOM file").unwrap();

    let inspection = inspect_input(&path).unwrap();

    assert!(inspection.instances.is_empty());
    assert!(inspection.warnings.is_empty());
}

#[test]
fn folder_scan_rejects_work_above_the_file_limit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.dcm"), b"first").unwrap();
    std::fs::write(dir.path().join("b.dcm"), b"second").unwrap();

    let err = candidate_paths_with_limit(dir.path(), SourceKind::Folder, 1).unwrap_err();
    assert!(
        matches!(&err, ViewerError::InvalidInput(message) if message.contains("more than 1 files")),
        "folder limit should return a clear invalid-input error: {err:?}"
    );
}

#[test]
fn warns_when_tiled_full_frame_count_mismatches_dense_grid() {
    let instance = DicomInstanceSummary {
        path: PathBuf::from("bad.dcm"),
        sop_class_uid: uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.into(),
        series_instance_uid_present: true,
        transfer_syntax_uid: uids::EXPLICIT_VR_LITTLE_ENDIAN.into(),
        image_type: vec![
            "ORIGINAL".into(),
            "PRIMARY".into(),
            "VOLUME".into(),
            "NONE".into(),
        ],
        rows: Some(2),
        columns: Some(2),
        total_pixel_matrix_rows: Some(5),
        total_pixel_matrix_columns: Some(5),
        number_of_frames: Some(8),
        optical_path_count: Some(1),
        focal_plane_count: Some(1),
        concatenation_instance_count: Some(1),
        pixel_spacing: None,
        dimension_organization_type: Some("TILED_FULL".into()),
        samples_per_pixel: Some(3),
        photometric_interpretation: Some("RGB".into()),
        planar_configuration: Some(0),
        bits_allocated: Some(8),
        bits_stored: Some(8),
        high_bit: Some(7),
        pixel_representation: Some(0),
    };
    let warnings = build_fact_warnings(&[instance], &[]);

    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("dense TILED_FULL grid expects 9")),
        "expected dense-grid frame warning, got {warnings:?}"
    );
}

#[test]
fn tiled_full_frame_warning_requires_all_dimension_counts() {
    let mut instance = DicomInstanceSummary {
        path: PathBuf::from("multidimensional.dcm"),
        sop_class_uid: uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.into(),
        series_instance_uid_present: true,
        transfer_syntax_uid: uids::EXPLICIT_VR_LITTLE_ENDIAN.into(),
        image_type: vec!["ORIGINAL".into(), "PRIMARY".into(), "VOLUME".into()],
        rows: Some(2),
        columns: Some(2),
        total_pixel_matrix_rows: Some(5),
        total_pixel_matrix_columns: Some(5),
        number_of_frames: Some(53),
        optical_path_count: None,
        focal_plane_count: Some(3),
        concatenation_instance_count: Some(1),
        pixel_spacing: None,
        dimension_organization_type: Some("TILED_FULL".into()),
        samples_per_pixel: Some(3),
        photometric_interpretation: Some("RGB".into()),
        planar_configuration: Some(0),
        bits_allocated: Some(8),
        bits_stored: Some(8),
        high_bit: Some(7),
        pixel_representation: Some(0),
    };

    assert!(build_fact_warnings(&[instance.clone()], &[])
        .iter()
        .all(|warning| !warning.contains("dense TILED_FULL")));

    instance.optical_path_count = Some(2);
    let warnings = build_fact_warnings(&[instance.clone()], &[]);
    assert!(warnings
        .iter()
        .any(|warning| warning.contains("expects 54")));

    instance.concatenation_instance_count = Some(2);
    assert!(build_fact_warnings(&[instance], &[])
        .iter()
        .all(|warning| !warning.contains("dense TILED_FULL")));
}

#[test]
fn opens_and_reads_first_tile_through_wsi_rs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.dcm");
    write_test_dicom(
        &path,
        "1.2.826.0.1.3680043.10.777.1",
        "1.2.826.0.1.3680043.10.777",
    );

    let study = ViewerStudy::open_path(&path).unwrap();
    assert_eq!(study.summary().format_label, "DICOM VL WSI");
    assert_eq!(study.summary().file_count, 1);
    assert_eq!(study.summary().dicom_instance_count, 1);
    assert_eq!(study.summary().levels.len(), 1);
    let tile = study
        .read_tile_rgba(LevelIndex::from_u32(0), TileCoord::new(0, 0))
        .unwrap();
    assert_eq!((tile.width, tile.height), (2, 2));
    assert_eq!(tile.rgba.len(), 2 * 2 * 4);
    assert_eq!(&tile.rgba[0..4], &[255, 0, 0, 255]);
}

#[test]
fn whole_level_layout_uses_viewer_display_tiles() {
    let layout = LevelTileLayout::WholeLevel {
        width: 12960,
        height: 9472,
        virtual_tile_width: 480,
        virtual_tile_height: 8,
    };

    assert_eq!(
        layout.display_tile_size(),
        (DEFAULT_DISPLAY_TILE_SIZE, DEFAULT_DISPLAY_TILE_SIZE)
    );
    assert_eq!(layout.grid_size(), Some((26, 19)));
    assert!(layout.contains(TileCoord::new(25, 18)));
    assert!(!layout.contains(TileCoord::new(26, 18)));
}

#[test]
#[ignore = "requires DICOM_VIEWER_WSI_FIXTURE to point at a local WSI file"]
fn opens_local_wsi_fixture_from_env() {
    let path = std::env::var_os("DICOM_VIEWER_WSI_FIXTURE")
        .map(PathBuf::from)
        .expect("DICOM_VIEWER_WSI_FIXTURE must point at a local WSI file");
    let study = ViewerStudy::open_path(&path).unwrap();
    let summary = study.summary();
    assert!(!summary.levels.is_empty());
    let overview = summary
        .levels
        .iter()
        .min_by_key(|level| u128::from(level.width) * u128::from(level.height))
        .expect("summary has levels");
    let tile = study
        .read_tile_rgba(overview.index, TileCoord::new(0, 0))
        .unwrap();
    assert!(tile.width > 0);
    assert!(tile.height > 0);
    assert_eq!(
        tile.rgba.len(),
        tile.width as usize * tile.height as usize * 4
    );
}

fn write_test_dicom(path: &Path, sop_instance_uid: &'static str, series_uid: &'static str) {
    let mut object = InMemDicomObject::new_empty();
    object.put(DataElement::new(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE,
    ));
    object.put(DataElement::new(
        tags::SOP_INSTANCE_UID,
        VR::UI,
        sop_instance_uid,
    ));
    object.put(DataElement::new(
        tags::SERIES_INSTANCE_UID,
        VR::UI,
        series_uid,
    ));
    object.put(DataElement::new(
        tags::IMAGE_TYPE,
        VR::CS,
        "ORIGINAL\\PRIMARY\\VOLUME\\NONE",
    ));
    object.put(DataElement::new(
        tags::ROWS,
        VR::US,
        PrimitiveValue::from(2u16),
    ));
    object.put(DataElement::new(
        tags::COLUMNS,
        VR::US,
        PrimitiveValue::from(2u16),
    ));
    object.put(DataElement::new(
        tags::TOTAL_PIXEL_MATRIX_ROWS,
        VR::UL,
        PrimitiveValue::from(2u32),
    ));
    object.put(DataElement::new(
        tags::TOTAL_PIXEL_MATRIX_COLUMNS,
        VR::UL,
        PrimitiveValue::from(2u32),
    ));
    object.put(DataElement::new(
        tags::NUMBER_OF_FRAMES,
        VR::IS,
        PrimitiveValue::from(1u32),
    ));
    object.put(DataElement::new(
        tags::DIMENSION_ORGANIZATION_TYPE,
        VR::CS,
        "TILED_FULL",
    ));
    object.put(DataElement::new(
        tags::SAMPLES_PER_PIXEL,
        VR::US,
        PrimitiveValue::from(3u16),
    ));
    object.put(DataElement::new(
        tags::PHOTOMETRIC_INTERPRETATION,
        VR::CS,
        "RGB",
    ));
    object.put(DataElement::new(
        tags::PLANAR_CONFIGURATION,
        VR::US,
        PrimitiveValue::from(0u16),
    ));
    object.put(DataElement::new(
        tags::BITS_ALLOCATED,
        VR::US,
        PrimitiveValue::from(8u16),
    ));
    object.put(DataElement::new(
        tags::BITS_STORED,
        VR::US,
        PrimitiveValue::from(8u16),
    ));
    object.put(DataElement::new(
        tags::HIGH_BIT,
        VR::US,
        PrimitiveValue::from(7u16),
    ));
    object.put(DataElement::new(
        tags::PIXEL_REPRESENTATION,
        VR::US,
        PrimitiveValue::from(0u16),
    ));
    object.put(DataElement::new(
        tags::PIXEL_SPACING,
        VR::DS,
        "0.00025\\0.00025",
    ));
    object.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PrimitiveValue::from(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]),
    ));
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .media_storage_sop_class_uid(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
                .media_storage_sop_instance_uid(sop_instance_uid)
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN),
        )
        .unwrap()
        .write_to_file(path)
        .unwrap();
}

#[cfg(any(target_os = "macos", feature = "cuda"))]
fn write_test_htj2k_dicom(path: &Path, codestream: Vec<u8>) {
    let sop_instance_uid = "1.2.826.0.1.3680043.10.777.2001";
    let series_uid = "1.2.826.0.1.3680043.10.777.2000";
    let mut object = InMemDicomObject::new_empty();
    for element in [
        DataElement::new(
            tags::SOP_CLASS_UID,
            VR::UI,
            PrimitiveValue::from(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE),
        ),
        DataElement::new(
            tags::SOP_INSTANCE_UID,
            VR::UI,
            PrimitiveValue::from(sop_instance_uid),
        ),
        DataElement::new(
            tags::SERIES_INSTANCE_UID,
            VR::UI,
            PrimitiveValue::from(series_uid),
        ),
        DataElement::new(
            tags::IMAGE_TYPE,
            VR::CS,
            PrimitiveValue::from("ORIGINAL\\PRIMARY\\VOLUME\\NONE"),
        ),
        DataElement::new(tags::ROWS, VR::US, PrimitiveValue::from(2_u16)),
        DataElement::new(tags::COLUMNS, VR::US, PrimitiveValue::from(2_u16)),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_ROWS,
            VR::UL,
            PrimitiveValue::from(2_u32),
        ),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_COLUMNS,
            VR::UL,
            PrimitiveValue::from(16_u32),
        ),
        DataElement::new(tags::NUMBER_OF_FRAMES, VR::IS, PrimitiveValue::from(8_u32)),
        DataElement::new(
            tags::DIMENSION_ORGANIZATION_TYPE,
            VR::CS,
            PrimitiveValue::from("TILED_FULL"),
        ),
        DataElement::new(tags::SAMPLES_PER_PIXEL, VR::US, PrimitiveValue::from(3_u16)),
        DataElement::new(
            tags::PHOTOMETRIC_INTERPRETATION,
            VR::CS,
            PrimitiveValue::from("RGB"),
        ),
        DataElement::new(
            tags::PLANAR_CONFIGURATION,
            VR::US,
            PrimitiveValue::from(0_u16),
        ),
        DataElement::new(tags::BITS_ALLOCATED, VR::US, PrimitiveValue::from(8_u16)),
        DataElement::new(tags::BITS_STORED, VR::US, PrimitiveValue::from(8_u16)),
        DataElement::new(tags::HIGH_BIT, VR::US, PrimitiveValue::from(7_u16)),
        DataElement::new(
            tags::PIXEL_REPRESENTATION,
            VR::US,
            PrimitiveValue::from(0_u16),
        ),
    ] {
        object.put(element);
    }
    let pixel_sequence = PixelFragmentSequence::from(
        (0..8)
            .map(|_| Fragments::new(codestream.clone(), 0))
            .collect::<Vec<_>>(),
    );
    object.put(DataElement::<InMemDicomObject>::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::from(pixel_sequence),
    ));
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .media_storage_sop_class_uid(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
                .media_storage_sop_instance_uid(sop_instance_uid)
                .transfer_syntax("1.2.840.10008.1.2.4.201"),
        )
        .unwrap()
        .write_to_file(path)
        .unwrap();
}
