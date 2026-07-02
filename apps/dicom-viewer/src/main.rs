mod app;

fn main() -> eframe::Result<()> {
    let initial_path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("WSI Viewer")
            .with_inner_size([1320.0, 880.0])
            .with_min_inner_size([900.0, 640.0]),
        vsync: true,
        hardware_acceleration: eframe::HardwareAcceleration::Required,
        renderer: eframe::Renderer::Glow,
        ..eframe::NativeOptions::default()
    };

    eframe::run_native(
        "WSI Viewer",
        options,
        Box::new(|cc| Ok(Box::new(app::DicomViewerApp::new(cc, initial_path)))),
    )
}
