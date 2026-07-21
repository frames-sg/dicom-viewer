use wsi_rs::{TileOutputPreference, TilePixels};

use crate::{
    RenderTile, Result, RgbaTile, TileDecodeBackend, ViewerCacheBudgets, ViewerError,
    ViewerOpenOptions,
};

const TILE_BACKEND_ENV: &str = "DICOM_VIEWER_TILE_BACKEND";
const MEMORY_PROFILE_ENV: &str = "DICOM_VIEWER_MEMORY_PROFILE";

pub(crate) fn rgba_tile_from_cpu_tile(tile: wsi_rs::CpuTile) -> Result<RgbaTile> {
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
    let cpu = if options.requests_cpu_only() {
        TileOutputPreference::cpu_only()
    } else {
        TileOutputPreference::cpu()
    };
    #[cfg(target_os = "macos")]
    if !options.requests_cpu_only() {
        if let Some(device) = options.metal_device() {
            let sessions = wsi_rs::output::metal::MetalBackendSessions::new(device.clone());
            let render =
                TileOutputPreference::prefer_device_auto_with_metal_and_compressed_decode(sessions);
            return (TileDecodeBackend::Metal, render, cpu);
        }
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
fn render_tile_from_device(_device: wsi_rs::DeviceTile) -> Result<RenderTile> {
    Err(ViewerError::Unsupported(
        "wsi-rs returned device-resident pixels without a configured viewer interop path".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(target_os = "macos")]
    #[test]
    fn auto_with_renderer_device_prefers_metal_but_keeps_cpu_compatibility_reads() {
        let Some(device) = metal::Device::system_default() else {
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
