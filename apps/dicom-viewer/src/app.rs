use std::path::PathBuf;
use std::sync::{mpsc::TryRecvError, Arc};
use std::time::Duration;

use dicom_viewer_core::{StudySummary, TileDecodeBackend, ViewerStudy};
use eframe::egui::{self, Frame, Rect, Sense};

mod camera;
mod canvas;
mod format;
mod measurement;
mod open_job;
mod theme;
mod tile;
mod ui;
mod viewport;

use camera::{wheel_zoom_factor, CameraState, CameraView};
#[cfg(test)]
use camera::{CameraMotion, MAX_ZOOM, MIN_ZOOM};
use canvas::{CanvasCamera, SlideCanvas};
use measurement::{
    clamp_base_point, draw_measurement_overlay, measurement_ready_status, MeasurementInteraction,
    MeasurementState,
};
use open_job::OpenJob;
use ui::chrome::{show_status_bar, show_toolbar};
use ui::facts::show_facts_sidebar;
use ui::overlay::{draw_canvas_overlays, paint_canvas_background, paint_empty_state, FrameStats};
use viewport::{base_contains_point, screen_to_base};
#[cfg(test)]
use viewport::{choose_render_level, visible_tiles};

#[cfg(test)]
use dicom_viewer_core::{LevelIndex, LevelInfo, SourceKind};
#[cfg(test)]
use eframe::egui::{pos2, vec2};
#[cfg(test)]
use ui::overlay::{fps_color, FrameRateInfo};

const MAX_LOADER_MESSAGES_PER_FRAME: usize = 128;
const LOADER_MESSAGE_BUDGET: Duration = Duration::from_millis(2);
const MAX_VISIBLE_UPLOADS_PER_FRAME: usize = 24;
const VISIBLE_UPLOAD_BUDGET: Duration = Duration::from_millis(6);
const MAX_PREFETCH_UPLOADS_PER_FRAME: usize = 4;
const PREFETCH_UPLOAD_BUDGET: Duration = Duration::from_millis(1);

pub struct DicomViewerApp {
    study: Option<Arc<ViewerStudy>>,
    open_job: Option<OpenJob>,
    active_generation: u64,
    next_generation: u64,
    status: String,
    canvas: SlideCanvas,
    frame_stats: FrameStats,
    camera: CameraState,
    show_facts_panel: bool,
    measurement: MeasurementState,
    reported_cpu_fallbacks: usize,
}

impl DicomViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        theme::install_visuals(&cc.egui_ctx);
        let render_state = cc
            .wgpu_render_state
            .clone()
            .expect("DICOM viewer requires the configured wgpu renderer");
        let canvas = SlideCanvas::new(render_state);
        let initial_status = canvas.backend_warning().map_or_else(
            || "Open a WSI file or DICOM folder.".to_string(),
            |warning| format!("Metal interop unavailable; using CPU → wgpu: {warning}"),
        );
        let mut app = Self {
            study: None,
            open_job: None,
            active_generation: 0,
            next_generation: 1,
            status: initial_status,
            canvas,
            frame_stats: FrameStats::default(),
            camera: CameraState::default(),
            show_facts_panel: false,
            measurement: MeasurementState::default(),
            reported_cpu_fallbacks: 0,
        };
        if let Some(path) = initial_path {
            app.start_open_path(path, &cc.egui_ctx);
        }
        app
    }

    fn start_open_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        let options = match self.canvas.viewer_open_options() {
            Ok(options) => options,
            Err(error) => {
                self.status = format!("Failed to open {}: {error}", path.display());
                return;
            }
        };
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        self.active_generation = generation;
        self.study = None;
        self.canvas.clear();
        self.camera.clear_for_open();
        self.measurement.reset();
        self.reported_cpu_fallbacks = 0;
        self.status = format!("Opening {}...", path.display());

        match OpenJob::spawn(path.clone(), generation, ctx, options) {
            Ok(job) => self.open_job = Some(job),
            Err(err) => {
                self.status = format!(
                    "Failed to open {}: could not start open worker: {err}",
                    path.display()
                );
            }
        }
        ctx.request_repaint();
    }

    fn poll_open_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.open_job else {
            return;
        };
        match job.receiver.try_recv() {
            Ok(result) => {
                let current_generation = self.active_generation;
                self.open_job = None;
                if result.generation != current_generation {
                    return;
                }
                match result.result {
                    Ok(study) => {
                        let tile_decode_backend = study.summary().tile_decode_backend;
                        self.camera.reset_for_study(study.summary());
                        self.study = Some(Arc::new(study));
                        self.canvas.clear();
                        let path_label = match tile_decode_backend {
                            TileDecodeBackend::Cpu => "CPU → wgpu",
                            TileDecodeBackend::Metal => "Metal preferred → wgpu",
                            TileDecodeBackend::Cuda => "CUDA preferred → wgpu",
                        };
                        self.status = format!("Opened {}. {path_label}.", result.path.display());
                    }
                    Err(err) => {
                        self.status = format!("Failed to open {}: {err}", result.path.display());
                    }
                }
                ctx.request_repaint();
            }
            Err(TryRecvError::Empty) => {
                // Don't spin — poll_open_job is called every frame from logic().
            }
            Err(TryRecvError::Disconnected) => {
                let path = job.path.clone();
                self.open_job = None;
                self.status = format!("Failed to open {}: open worker exited", path.display());
            }
        }
    }

    fn pick_file(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "WSI",
                &[
                    "dcm", "dicom", "svs", "tif", "tiff", "ndpi", "scn", "bif", "czi", "zvi",
                    "mrxs", "vms", "vmu", "vsi", "svcache", "j2k", "j2c",
                ],
            )
            .add_filter("DICOM", &["dcm", "dicom"])
            .add_filter("TIFF WSI", &["svs", "tif", "tiff", "ndpi", "scn", "bif"])
            .add_filter("JPEG 2000", &["j2k", "j2c"])
            .pick_file()
        {
            self.start_open_path(path, ctx);
        }
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.start_open_path(path, ctx);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input_mut(|input| std::mem::take(&mut input.raw.dropped_files));
        for file in dropped {
            if let Some(path) = file.path {
                self.start_open_path(path, ctx);
                break;
            }
        }
    }

    fn handle_measure_tool_clicked(&mut self, summary: Option<&StudySummary>, had_points: bool) {
        if had_points {
            self.measurement.clear_points();
            self.measurement.active = true;
            self.status = measurement_ready_status(summary);
        } else if self.measurement.active {
            self.measurement.clear_points();
            self.status = measurement_ready_status(summary);
        } else {
            self.measurement.clear_points();
            self.status = "Measurement cleared.".to_string();
        }
    }

    fn handle_measurement_interaction(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
        summary: &StudySummary,
        view: CameraView,
    ) -> MeasurementInteraction {
        let mut interaction = MeasurementInteraction::default();
        if !self.measurement.active {
            self.measurement.dragging = None;
            return interaction;
        }

        let primary_down = ui.input(|input| input.pointer.primary_down());
        if !primary_down {
            self.measurement.dragging = None;
        }

        if response.drag_started() {
            if let Some(pointer) = response.interact_pointer_pos() {
                if let Some(index) = self.measurement.hit_test(rect, pointer, view) {
                    self.measurement.dragging = Some(index);
                    interaction.drag_consumed = true;
                    response.request_focus();
                }
            }
        }

        if let Some(index) = self.measurement.dragging {
            if let Some(pointer) = ui.input(|input| input.pointer.interact_pos()) {
                let point = screen_to_base(rect, pointer, view.center_base, view.zoom);
                self.measurement
                    .set_point(index, clamp_base_point(summary, point));
                if let Some(label) = self.measurement.distance_label(summary) {
                    self.status = format!("Measured {label}.");
                }
                ui.ctx().request_repaint();
            }
            interaction.drag_consumed = true;
        }

        if response.clicked() {
            interaction.click_consumed = true;
            if let Some(pointer) = response.interact_pointer_pos() {
                if self.measurement.hit_test(rect, pointer, view).is_none() {
                    let point = screen_to_base(rect, pointer, view.center_base, view.zoom);
                    if base_contains_point(summary, point) {
                        self.measurement.place_next_point(point);
                        if let Some(label) = self.measurement.distance_label(summary) {
                            self.status = format!("Measured {label}.");
                        } else {
                            self.status = "Measurement point set.".to_string();
                        }
                        ui.ctx().request_repaint();
                    }
                }
            }
        }

        interaction
    }
}

impl eframe::App for DicomViewerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_dropped_files(ctx);
        self.poll_open_job(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let (stable_dt, predicted_dt) = ui.input(|input| (input.stable_dt, input.predicted_dt));
        self.frame_stats.record(stable_dt, predicted_dt);

        let study = self.study.clone();
        let opening = self.open_job.is_some();

        let had_measurement = self.measurement.has_points();
        let actions = show_toolbar(
            ui,
            study.is_some(),
            &mut self.show_facts_panel,
            &mut self.measurement.active,
            self.camera.smoothing_enabled_mut(),
        );
        if actions.open_file {
            self.pick_file(ui.ctx());
        }
        if actions.open_folder {
            self.pick_folder(ui.ctx());
        }
        if actions.measure_clicked {
            self.handle_measure_tool_clicked(
                study.as_ref().map(|study| study.summary()),
                had_measurement,
            );
        }
        show_status_bar(
            ui,
            &self.status,
            opening,
            study.as_ref().map(|study| study.summary()),
        );
        show_facts_sidebar(
            ui,
            self.show_facts_panel,
            study.as_ref().map(|study| study.summary()),
        );

        // ── Central canvas ─────────────────────────────────────────
        egui::CentralPanel::default_margins()
            .frame(Frame::NONE.fill(theme::CANVAS))
            .show_inside(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                let response = ui.allocate_rect(rect, Sense::click_and_drag());
                if response.clicked() || response.drag_started() {
                    response.request_focus();
                } else if ui.input(|input| input.pointer.any_pressed()) && !response.hovered() {
                    response.surrender_focus();
                }
                let painter = ui.painter_at(rect);
                paint_canvas_background(&painter, rect);

                let Some(study) = study.clone() else {
                    paint_empty_state(&painter, rect, opening);
                    return;
                };

                if actions.fit {
                    self.camera.request_fit();
                }
                self.camera.prepare_canvas(rect, study.summary());
                if actions.zoom_out {
                    self.camera.zoom_about_center(rect, 0.8);
                }
                if actions.zoom_in {
                    self.camera.zoom_about_center(rect, 1.25);
                }

                let measurement_view = self.camera.target_view();
                let measurement_interaction = self.handle_measurement_interaction(
                    ui,
                    &response,
                    rect,
                    study.summary(),
                    measurement_view,
                );

                if response.dragged() && !measurement_interaction.drag_consumed {
                    self.camera.pan_by(response.drag_delta());
                    ui.ctx().request_repaint();
                }
                if response.double_clicked() && !measurement_interaction.click_consumed {
                    let pointer = response.interact_pointer_pos().unwrap_or(rect.center());
                    self.camera.zoom_around(rect, pointer, 2.0);
                    ui.ctx().request_repaint();
                }

                if response.hovered() {
                    let scroll_y = ui.input(|input| input.smooth_scroll_delta.y);
                    if scroll_y.abs() > 0.0 {
                        let pointer = ui
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(rect.center());
                        self.camera
                            .zoom_around(rect, pointer, wheel_zoom_factor(scroll_y));
                        ui.ctx().request_repaint();
                    }
                    let pinch = ui.input(|input| input.zoom_delta());
                    if (pinch - 1.0).abs() > 0.001 {
                        let pointer = ui
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(rect.center());
                        self.camera.zoom_around(rect, pointer, pinch);
                        ui.ctx().request_repaint();
                    }
                    if ui.input(|input| input.pointer.any_down()) {
                        ui.ctx().request_repaint();
                    }
                }

                let accepts_keys =
                    (response.hovered() || response.has_focus()) && !ui.ctx().text_edit_focused();
                if self.camera.handle_keys(ui, rect, accepts_keys) {
                    ui.ctx().request_repaint();
                }
                let (render_view, animating_camera) =
                    self.camera.render_view(rect, study.summary(), stable_dt);
                let target_view = self.camera.target_view();
                if animating_camera {
                    ui.ctx().request_repaint();
                }
                self.canvas.paint(
                    ui.ctx(),
                    &painter,
                    rect,
                    &study,
                    self.active_generation,
                    CanvasCamera {
                        rendered: render_view,
                        target: target_view,
                        camera_animating: animating_camera,
                    },
                );
                if let Some((count, reason)) = self.canvas.cpu_fallback() {
                    if count > self.reported_cpu_fallbacks {
                        self.reported_cpu_fallbacks = count;
                        self.status = format!(
                            "Metal preferred → wgpu; CPU fallback used for {count} tile(s): {reason}"
                        );
                    }
                }

                let hover_base = response
                    .hover_pos()
                    .map(|p| screen_to_base(rect, p, render_view.center_base, render_view.zoom));
                let tile_failure = self.canvas.tile_failure();
                draw_canvas_overlays(
                    &painter,
                    rect,
                    study.summary(),
                    render_view.zoom,
                    self.frame_stats.info(),
                    hover_base,
                    tile_failure,
                );
                draw_measurement_overlay(
                    &painter,
                    rect,
                    study.summary(),
                    &self.measurement,
                    hover_base,
                    render_view.center_base,
                    render_view.zoom,
                );
            });
    }
}

#[cfg(test)]
mod tests;
