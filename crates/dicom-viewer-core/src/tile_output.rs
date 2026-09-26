use wsi_rs::{TileOutputPreference, TilePixels};

use crate::{
    RenderTile, Result, RgbaTile, TileDecodeBackend, ViewerCacheBudgets, ViewerError,
    ViewerOpenOptions,
};

const TILE_BACKEND_ENV: &str = "DICOM_VIEWER_TILE_BACKEND";
const MEMORY_PROFILE_ENV: &str = "DICOM_VIEWER_MEMORY_PROFILE";

pub(crate) fn rgba_tile_from_cpu_tile(tile: wsi_rs::CpuTile) -> Result<RgbaTile> {
    // The renderer always consumes interleaved RGBA8. Specialize the common
    // RGB8 layout once, outside the loop, so LLVM can vectorize expansion.
    // All other layouts, color spaces, and sample types use the source adapter.
    if tile.channels() == 3
        && *tile.color_space() == wsi_rs::ColorSpace::Rgb
        && tile.layout() == wsi_rs::CpuTileLayout::Interleaved
    {
        if let Some(rgb) = tile.as_u8() {
            let pixels = (tile.width() as usize).checked_mul(tile.height() as usize);
            if let Some(byte_len) = pixels.and_then(|n| n.checked_mul(4)) {
                if pixels.and_then(|n| n.checked_mul(3)) == Some(rgb.len()) {
                    let mut rgba = vec![255; byte_len];
                    for (source, destination) in rgb.chunks_exact(3).zip(rgba.chunks_exact_mut(4)) {
                        destination[..3].copy_from_slice(source);
                    }
                    return Ok(RgbaTile {
                        width: tile.width(),
                        height: tile.height(),
                        rgba,
                    });
                }
            }
        }
    }
    let image = tile.into_rgba()?;
    Ok(RgbaTile {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

pub(crate) fn rgba_tile_from_pixels(tile: TilePixels) -> Result<RgbaTile> {
    match tile {
        TilePixels::Cpu(tile) => rgba_tile_from_cpu_tile(tile),
        TilePixels::Device(_) => Err(ViewerError::Unsupported(
            "wsi-rs violated the viewer tile-output contract by returning device-resident pixels"
                .into(),
        )),
        #[allow(unreachable_patterns)]
        _ => Err(ViewerError::Unsupported(
            "wsi-rs returned an unknown tile pixel output variant".into(),
        )),
    }
}

pub(crate) fn default_viewer_open_options() -> Result<ViewerOpenOptions> {
    let requested = std::env::var(TILE_BACKEND_ENV).unwrap_or_else(|_| "auto".into());
    let options = viewer_open_options(&requested)?;
    Ok(options.with_cache_budgets(default_cache_budgets()?))
}

pub(crate) fn default_cache_budgets() -> Result<ViewerCacheBudgets> {
    let profile = std::env::var(MEMORY_PROFILE_ENV).unwrap_or_else(|_| "balanced".into());
    memory_profile_cache_budgets(&profile)
}

fn memory_profile_cache_budgets(profile: &str) -> Result<ViewerCacheBudgets> {
    match profile.to_ascii_lowercase().as_str() {
        "balanced" => Ok(ViewerCacheBudgets::balanced()),
        "large" => Ok(ViewerCacheBudgets::large()),
        other => Err(ViewerError::InvalidInput(format!(
            "{MEMORY_PROFILE_ENV} only supports balanced or large; got {other:?}"
        ))),
    }
}

fn viewer_open_options(requested: &str) -> Result<ViewerOpenOptions> {
    match requested.to_ascii_lowercase().as_str() {
        "auto" => Ok(ViewerOpenOptions::auto()),
        "cpu" => Ok(ViewerOpenOptions::cpu_only()),
        other => Err(ViewerError::InvalidInput(format!(
            "{TILE_BACKEND_ENV} only supports auto or cpu; got {other:?}"
        ))),
    }
}

pub(crate) fn tile_output_config(
    options: &ViewerOpenOptions,
) -> (
    TileDecodeBackend,
    TileOutputPreference,
    TileOutputPreference,
) {
    #[cfg(not(any(target_os = "macos", feature = "cuda")))]
    let _ = options;
    // Compatibility reads and ordered device-failure retries must never
    // select the same device backend that just failed its download boundary.
    let cpu = TileOutputPreference::cpu_only();
    #[cfg(target_os = "macos")]
    if !options.requests_cpu_only() {
        if let Some(device) = options.metal_device() {
            let sessions = wsi_rs::output::metal::MetalBackendSessions::new(device.clone());
            let render =
                TileOutputPreference::prefer_device_auto_with_metal_and_compressed_decode(sessions);
            return (TileDecodeBackend::Metal, render, cpu);
        }
    }
    #[cfg(all(feature = "cuda", not(target_os = "macos")))]
    if !options.requests_cpu_only() {
        let sessions = wsi_rs::output::cuda::CudaBackendSessions::new();
        let render =
            TileOutputPreference::prefer_device_auto_with_cuda_and_compressed_decode(sessions);
        return (TileDecodeBackend::Cuda, render, cpu);
    }
    (TileDecodeBackend::Cpu, cpu.clone(), cpu)
}

pub(crate) fn render_tile_from_pixels(tile: TilePixels) -> Result<RenderTile> {
    match tile {
        TilePixels::Cpu(tile) => rgba_tile_from_cpu_tile(tile).map(RenderTile::Cpu),
        TilePixels::Device(device) => render_tile_from_device(device),
        #[allow(unreachable_patterns)]
        _ => Err(ViewerError::Unsupported(
            "wsi-rs returned an unknown tile pixel output variant".into(),
        )),
    }
}

#[cfg(target_os = "macos")]
fn render_tile_from_device(device: wsi_rs::DeviceTile) -> Result<RenderTile> {
    match device {
        wsi_rs::DeviceTile::Metal(tile) => crate::MetalRenderTile::new(tile).map(RenderTile::Metal),
        #[allow(unreachable_patterns)]
        _ => Err(ViewerError::Unsupported(
            "wsi-rs returned a device tile unsupported by the current wgpu renderer".into(),
        )),
    }
}

#[cfg(not(target_os = "macos"))]
fn render_tile_from_device(device: wsi_rs::DeviceTile) -> Result<RenderTile> {
    match device {
        #[cfg(feature = "cuda")]
        wsi_rs::DeviceTile::Cuda(tile) => {
            rgba_tile_from_cpu_tile(tile.download_cpu()?).map(RenderTile::Cpu)
        }
        #[allow(unreachable_patterns)]
        _ => Err(ViewerError::Unsupported(
            "wsi-rs returned device-resident pixels without a configured viewer download path"
                .into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_expansion_matches_source_adapter_and_rgba_preserves_ownership() {
        for (width, height) in [(1, 1), (2, 1), (17, 3), (256, 256), (257, 3)] {
            let bytes = (0..width * height * 3).map(|i| (i % 256) as u8).collect();
            let tile = wsi_rs::CpuTile::from_u8_interleaved(
                width,
                height,
                3,
                wsi_rs::ColorSpace::Rgb,
                bytes,
            )
            .unwrap();
            let expected = tile.to_rgba().unwrap();
            let shared = tile.clone();
            let actual = rgba_tile_from_cpu_tile(tile).unwrap();
            assert_eq!(actual.rgba, expected.into_raw());
            assert_eq!(actual.width, width);
            assert_eq!(actual.height, height);
            assert_eq!(shared.to_rgba().unwrap().into_raw(), actual.rgba);
        }
        let planar = wsi_rs::CpuTile::new(
            2,
            1,
            3,
            wsi_rs::ColorSpace::Rgb,
            wsi_rs::CpuTileLayout::Planar,
            wsi_rs::CpuTileData::u8(vec![10, 40, 20, 50, 30, 60]),
        )
        .unwrap();
        assert_eq!(
            rgba_tile_from_cpu_tile(planar).unwrap().rgba,
            [10, 20, 30, 255, 40, 50, 60, 255]
        );
        let bytes = vec![10, 20, 30, 47];
        let pointer = bytes.as_ptr();
        let tile =
            wsi_rs::CpuTile::from_u8_interleaved(1, 1, 4, wsi_rs::ColorSpace::Rgba, bytes).unwrap();
        let actual = rgba_tile_from_cpu_tile(tile).unwrap();
        assert_eq!(actual.rgba, [10, 20, 30, 47]);
        assert_eq!(actual.rgba.as_ptr(), pointer);
    }

    #[cfg(any(not(feature = "cuda"), target_os = "macos"))]
    #[test]
    fn auto_requests_host_resident_output() {
        let options = viewer_open_options("auto").unwrap();
        let (backend, preference, cpu) = tile_output_config(&options);

        assert_eq!(backend, TileDecodeBackend::Cpu);
        assert!(!preference.prefers_device());
        assert!(!cpu.prefers_device());
    }

    #[test]
    fn obsolete_device_residency_selection_is_rejected() {
        let err = viewer_open_options("metal").unwrap_err();

        assert!(
            matches!(&err, ViewerError::InvalidInput(message) if message.contains("only supports auto or cpu") && message.contains("metal")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn balanced_and_large_memory_profiles_have_explicit_budgets() {
        assert_eq!(
            memory_profile_cache_budgets("balanced").unwrap(),
            ViewerCacheBudgets::new(256 * 1024 * 1024, 128 * 1024 * 1024, 32 * 1024 * 1024)
        );
        assert_eq!(
            memory_profile_cache_budgets("large").unwrap(),
            ViewerCacheBudgets::new(512 * 1024 * 1024, 256 * 1024 * 1024, 64 * 1024 * 1024)
        );
        assert!(memory_profile_cache_budgets("huge").is_err());
    }

    #[cfg(all(feature = "cuda", not(target_os = "macos")))]
    #[test]
    fn cuda_feature_auto_prefers_reusable_compressed_decode_sessions() {
        let options = ViewerOpenOptions::auto();

        let (backend, render, cpu) = tile_output_config(&options);

        assert_eq!(backend, TileDecodeBackend::Cuda);
        assert!(render.prefers_device());
        assert!(render.compressed_device_decode_enabled());
        assert!(render.adaptive_decode_route_enabled());
        assert!(!cpu.prefers_device());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn auto_with_renderer_device_prefers_metal_but_keeps_cpu_compatibility_reads() {
        let Ok(device) = j2k_metal_support::system_default_device() else {
            return;
        };
        let options = ViewerOpenOptions::auto().with_metal_device(device);

        let (backend, render, cpu) = tile_output_config(&options);

        assert_eq!(backend, TileDecodeBackend::Metal);
        assert!(render.prefers_device());
        assert!(render.compressed_device_decode_enabled());
        assert!(render.adaptive_decode_route_enabled());
        assert!(!cpu.prefers_device());
    }
}
