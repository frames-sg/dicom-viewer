use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dicom_viewer_core::{StudySummary, TileDecodeBackend, ViewerStudy};
use eframe::egui::{self, Frame, Rect, Sense};

mod annotation;
mod camera;
mod canvas;
mod format;
mod level_warmer;
mod measurement;
mod open_job;
mod theme;
mod tile;
mod ui;
mod viewport;

use annotation::{draw_annotation_overlay, AnnotationState};
use camera::{wheel_zoom_factor, CameraState, CameraView};
#[cfg(test)]
use camera::{CameraMotion, MAX_ZOOM, MIN_ZOOM};
use canvas::SlideCanvas;
use measurement::{
    clamp_base_point, draw_measurement_overlay, measurement_ready_status, MeasurementInteraction,
    MeasurementState,
};
use open_job::{OpenPoll, OpenQueue};
use ui::chrome::{show_status_bar, show_toolbar, ToolbarState};
use ui::facts::show_facts_sidebar;
use ui::overlay::{
    draw_canvas_overlays, paint_canvas_background, paint_empty_state, FrameStats, OverlayInfo,
};
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
const MAX_TRANSITION_UPLOADS_PER_FRAME: usize = 2;
const MAX_PREFETCH_UPLOADS_PER_FRAME: usize = 4;
const PREFETCH_UPLOAD_BUDGET: Duration = Duration::from_millis(1);

pub struct DicomViewerApp {
    study: Option<Arc<ViewerStudy>>,
    open_queue: OpenQueue,
    active_generation: u64,
    next_generation: u64,
    status: String,
    canvas: SlideCanvas,
    frame_stats: FrameStats,
    camera: CameraState,
    show_facts_panel: bool,
    measurement: MeasurementState,
    annotations: AnnotationState,
    active_path: Option<PathBuf>,
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
            open_queue: OpenQueue::default(),
            active_generation: 0,
            next_generation: 1,
            status: initial_status,
            canvas,
            frame_stats: FrameStats::default(),
            camera: CameraState::default(),
            show_facts_panel: false,
            measurement: MeasurementState::default(),
            annotations: AnnotationState::default(),
            active_path: None,
            reported_cpu_fallbacks: 0,
        };
        if let Some(path) = initial_path {
            app.start_open_path(path, &cc.egui_ctx);
        }
        app
    }

    fn start_open_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        if !self.confirm_discard_annotations(
            "Opening another slide will discard the annotations that have not been saved.",
        ) {
            self.status = "Open cancelled; annotations were kept.".to_string();
            return;
        }
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
        self.annotations.reset();
        self.active_path = None;
        self.reported_cpu_fallbacks = 0;
        self.status = format!("Opening {}...", path.display());

        match self
            .open_queue
            .submit(path.clone(), generation, ctx, options)
        {
            Ok(()) => {}
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
        match self.open_queue.poll(ctx) {
            Ok(Some(OpenPoll::Result(result))) => {
                let current_generation = self.active_generation;
                if result.generation != current_generation {
                    return;
                }
                match result.result {
                    Ok(study) => {
                        let tile_decode_backend = study.summary().tile_decode_backend;
                        self.camera.reset_for_study(study.summary());
                        self.study = Some(Arc::new(study));
                        self.active_path = Some(result.path.clone());
                        self.canvas.clear();
                        let path_label = match tile_decode_backend {
                            TileDecodeBackend::Cpu => "CPU → wgpu",
                            TileDecodeBackend::Metal => "Metal preferred → wgpu",
                            TileDecodeBackend::Cuda => "CUDA preferred → wgpu",
                        };
                        self.status = format!("Opened {}. {path_label}.", result.path.display());
                    }
                    Err(err) => {
                        self.active_path = None;
                        self.status = format!("Failed to open {}: {err}", result.path.display());
                    }
                }
                ctx.request_repaint();
            }
            Ok(Some(OpenPoll::Disconnected(path))) => {
                self.status = format!("Failed to open {}: open worker exited", path.display());
            }
            Ok(None) => {}
            Err(error) => {
                self.status = format!("Failed to start pending open worker: {error}");
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

    fn confirm_discard_annotations(&self, description: &str) -> bool {
        !self.annotations.has_unsaved_work()
            || rfd::MessageDialog::new()
                .set_title("Discard unsaved annotations?")
                .set_description(description)
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                == rfd::MessageDialogResult::Yes
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

    fn handle_annotation_interaction(
        &mut self,
        response: &egui::Response,
        rect: Rect,
        summary: &StudySummary,
        view: CameraView,
    ) -> bool {
        if !self.annotations.active {
            return false;
        }

        if response.double_clicked() {
            match self.annotations.close_current() {
                Ok(()) => {
                    self.status = format!("{} polygon closed.", self.annotations.mode.label());
                }
                Err(error) => self.status = error.to_string(),
            }
            response.request_focus();
            return true;
        }

        if response.clicked() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let point = screen_to_base(rect, pointer, view.center_base, view.zoom);
                if base_contains_point(summary, point) {
                    self.annotations.add_vertex(point);
                    self.status = format!(
                        "{} vertex added; double-click or use Close to finish.",
                        self.annotations.mode.label()
                    );
                    response.request_focus();
                }
            }
            return true;
        }
        false
    }

    fn close_annotation_polygon(&mut self) {
        match self.annotations.close_current() {
            Ok(()) => {
                self.status = format!("{} polygon closed.", self.annotations.mode.label());
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn export_annotations(&mut self) {
        let default_name = self
            .active_path
            .as_deref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .map_or_else(
                || "viable_tumor.geojson".to_string(),
                |stem| format!("{stem}_viable_tumor.geojson"),
            );
        let mut dialog = rfd::FileDialog::new()
            .add_filter("GeoJSON", &["geojson"])
            .set_file_name(&default_name);
        if let Some(parent) = self.active_path.as_deref().and_then(|path| path.parent()) {
            dialog = dialog.set_directory(parent);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        match self.annotations.save_geojson(&path) {
            Ok(()) => {
                self.annotations.mark_saved();
                self.status = format!(
                    "Saved {} viable-tumor fragment(s) to {}.",
                    self.annotations.completed_tumor_count(),
                    path.display()
                );
            }
            Err(error) => self.status = error.to_string(),
        }
    }
}

impl eframe::App for DicomViewerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.viewport().close_requested())
            && !self.confirm_discard_annotations(
                "Closing the viewer will discard the annotations that have not been saved.",
            )
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.status = "Close cancelled; annotations were kept.".to_string();
        }
        self.handle_dropped_files(ctx);
        self.poll_open_job(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let app_ui_started = self.canvas.debug_stats_enabled().then(Instant::now);
        let (stable_dt, predicted_dt) = ui.input(|input| (input.stable_dt, input.predicted_dt));
        self.frame_stats.record(stable_dt, predicted_dt);

        let has_study = self.study.is_some();

        let had_measurement = self.measurement.has_points();
        let annotation_mode = self.annotations.mode;
        let has_open_polygon = self.annotations.has_current_vertices();
        let has_exportable_annotations = self.annotations.completed_tumor_count() > 0;
        let actions = show_toolbar(
            ui,
            ToolbarState {
                has_study,
                show_facts: &mut self.show_facts_panel,
                measurement_active: &mut self.measurement.active,
                annotation_active: &mut self.annotations.active,
                annotation_mode,
                has_annotation_work: has_open_polygon || has_exportable_annotations,
                has_open_polygon,
                has_exportable_annotations,
                smooth_camera: self.camera.smoothing_enabled_mut(),
            },
        );
        if actions.open_file {
            self.pick_file(ui.ctx());
        }
        if actions.open_folder {
            self.pick_folder(ui.ctx());
        }
        // Opening replaces the active study and generation. Refresh the frame
        // snapshot after the modal picker returns so this frame cannot submit
        // work for the previous study under the replacement generation.
        let study = self.study.clone();
        let opening = self.open_queue.is_opening();
        if actions.measure_clicked {
            if self.measurement.active {
                self.annotations.active = false;
            }
            self.handle_measure_tool_clicked(
                study.as_ref().map(|study| study.summary()),
                had_measurement,
            );
        }
        if actions.annotate_clicked {
            if self.annotations.active {
                self.measurement.active = false;
                self.status =
                    "Annotation active; click vertices and double-click or use Close.".to_string();
            } else {
                self.status = "Annotation paused.".to_string();
            }
        }
        if let Some(mode) = actions.annotation_mode {
            let discarded = self.annotations.set_mode(mode);
            self.status = if discarded {
                format!(
                    "{} mode active; unfinished polygon discarded.",
                    mode.label()
                )
            } else {
                format!("{} mode active.", mode.label())
            };
        }
        if actions.close_polygon {
            self.close_annotation_polygon();
        }
        if actions.undo_annotation && self.annotations.undo() {
            self.status = "Last annotation step undone.".to_string();
        }
        if actions.export_annotations {
            self.export_annotations();
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
            self.active_generation,
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
                    self.canvas.record_zoom_input();
                    self.camera.request_fit();
                }
                self.camera.prepare_canvas(rect, study.summary());
                if actions.zoom_out {
                    self.canvas.record_zoom_input();
                    self.camera.zoom_about_center(rect, 0.8);
                }
                if actions.zoom_in {
                    self.canvas.record_zoom_input();
                    self.camera.zoom_about_center(rect, 1.25);
                }

                let accepts_keys =
                    (response.hovered() || response.has_focus()) && !ui.ctx().text_edit_focused();
                let zoom_before_keys = self.camera.target_view().zoom;
                if self.camera.handle_keys(ui, rect, accepts_keys) {
                    if (self.camera.target_view().zoom - zoom_before_keys).abs() > f32::EPSILON {
                        self.canvas.record_zoom_input();
                    }
                    ui.ctx().request_repaint();
                }

                let camera_frame = self.camera.frame(rect, study.summary(), stable_dt);
                if camera_frame.animating {
                    ui.ctx().request_repaint();
                }
                let measurement_interaction = self.handle_measurement_interaction(
                    ui,
                    &response,
                    rect,
                    study.summary(),
                    camera_frame.rendered,
                );
                let annotation_click_consumed = self.handle_annotation_interaction(
                    &response,
                    rect,
                    study.summary(),
                    camera_frame.rendered,
                );

                if response.dragged() && !measurement_interaction.drag_consumed {
                    self.camera
                        .pan_by_rendered(response.drag_delta(), camera_frame.rendered);
                    ui.ctx().request_repaint();
                }
                if response.double_clicked()
                    && !measurement_interaction.click_consumed
                    && !annotation_click_consumed
                {
                    let pointer = response.interact_pointer_pos().unwrap_or(rect.center());
                    self.canvas.record_zoom_input();
                    self.camera
                        .zoom_around_rendered(rect, pointer, 2.0, camera_frame.rendered);
                    ui.ctx().request_repaint();
                }

                if response.hovered() {
                    let scroll_y = ui.input(|input| input.smooth_scroll_delta.y);
                    if scroll_y.abs() > 0.0 {
                        let pointer = ui
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(rect.center());
                        self.canvas.record_zoom_input();
                        self.camera.zoom_around_rendered(
                            rect,
                            pointer,
                            wheel_zoom_factor(scroll_y),
                            camera_frame.rendered,
                        );
                        ui.ctx().request_repaint();
                    }
                    let pinch = ui.input(|input| input.zoom_delta());
                    if (pinch - 1.0).abs() > 0.001 {
                        let pointer = ui
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(rect.center());
                        self.canvas.record_zoom_input();
                        self.camera.zoom_around_rendered(
                            rect,
                            pointer,
                            pinch,
                            camera_frame.rendered,
                        );
                        ui.ctx().request_repaint();
                    }
                    if ui.input(|input| input.pointer.any_down()) {
                        ui.ctx().request_repaint();
                    }
                }

                self.canvas.paint(
                    ui.ctx(),
                    &painter,
                    rect,
                    &study,
                    self.active_generation,
                    camera_frame,
                );
                if let Some((count, reason)) = self.canvas.cpu_fallback() {
                    if count > self.reported_cpu_fallbacks {
                        self.reported_cpu_fallbacks = count;
                        self.status = format!(
                            "{} preferred → wgpu; CPU fallback used for {count} tile(s): {reason}",
                            study.summary().tile_decode_backend
                        );
                    }
                }

                let hover_base = response.hover_pos().map(|p| {
                    screen_to_base(
                        rect,
                        p,
                        camera_frame.rendered.center_base,
                        camera_frame.rendered.zoom,
                    )
                });
                let tile_failure = self.canvas.tile_failure();
                let debug_stats = self.canvas.debug_stats_text();
                draw_canvas_overlays(
                    &painter,
                    rect,
                    OverlayInfo {
                        summary: study.summary(),
                        zoom: camera_frame.rendered.zoom,
                        frame_rate: self.frame_stats.info(),
                        hover_base,
                        tile_failure,
                        debug_stats: debug_stats.as_deref(),
                    },
                );
                draw_measurement_overlay(
                    &painter,
                    rect,
                    study.summary(),
                    &self.measurement,
                    hover_base,
                    camera_frame.rendered.center_base,
                    camera_frame.rendered.zoom,
                );
                draw_annotation_overlay(
                    &painter,
                    rect,
                    &self.annotations,
                    hover_base.filter(|point| base_contains_point(study.summary(), *point)),
                    camera_frame.rendered,
                );
            });
        if let Some(started) = app_ui_started {
            self.canvas.record_app_ui_cpu_time(started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests;
