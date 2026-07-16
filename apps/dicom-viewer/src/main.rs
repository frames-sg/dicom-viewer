#![forbid(unsafe_code)]

mod app;

fn main() -> eframe::Result<()> {
    let initial_path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = native_options();

    eframe::run_native(
        "WSI Viewer",
        options,
        Box::new(|cc| Ok(Box::new(app::DicomViewerApp::new(cc, initial_path)))),
    )
}

fn native_options() -> eframe::NativeOptions {
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration {
        present_mode: eframe::wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: Some(1),
        ..eframe::egui_wgpu::WgpuConfiguration::default()
    };
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = platform_wgpu_backends();
    }

    eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("WSI Viewer")
            .with_inner_size([1320.0, 880.0])
            .with_min_inner_size([900.0, 640.0]),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        ..eframe::NativeOptions::default()
    }
}

fn platform_wgpu_backends() -> eframe::wgpu::Backends {
    wgpu_backends_for(std::env::consts::OS)
}

fn wgpu_backends_for(os: &str) -> eframe::wgpu::Backends {
    match os {
        "macos" => eframe::wgpu::Backends::METAL,
        "windows" => eframe::wgpu::Backends::DX12,
        "linux" => eframe::wgpu::Backends::VULKAN.union(eframe::wgpu::Backends::GL),
        _ => eframe::wgpu::Backends::PRIMARY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_viewer_uses_low_latency_wgpu() {
        let options = native_options();

        assert_eq!(options.renderer.to_string(), "wgpu");
        assert_eq!(
            options.wgpu_options.present_mode,
            eframe::wgpu::PresentMode::AutoVsync
        );
        assert_eq!(options.wgpu_options.desired_maximum_frame_latency, Some(1));
        let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = options.wgpu_options.wgpu_setup else {
            panic!("viewer should create its platform wgpu device");
        };
        assert_eq!(setup.instance_descriptor.backends, platform_wgpu_backends());
    }

    #[test]
    fn renderer_backend_policy_is_explicit_for_every_desktop_platform() {
        assert_eq!(wgpu_backends_for("macos"), eframe::wgpu::Backends::METAL);
        assert_eq!(wgpu_backends_for("windows"), eframe::wgpu::Backends::DX12);
        assert_eq!(
            wgpu_backends_for("linux"),
            eframe::wgpu::Backends::VULKAN.union(eframe::wgpu::Backends::GL)
        );
    }
}
