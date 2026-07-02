use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
    Arc,
};
use std::thread;
use std::time::Duration;

use dicom_viewer_core::{LevelInfo, StudySummary, ViewerStudy};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Panel, Rect,
    RichText, Sense, Stroke, StrokeKind, Vec2,
};

mod theme;
mod tile;
mod ui;
mod viewport;

use tile::{QueueLane, TilePollRequest, TileRenderer, VisibleTile};
use ui::{
    chrome_frame, draw_canvas_overlays, empty_facts, facts_panel, install_visuals,
    paint_canvas_background, paint_empty_state, privacy_badge, rule, status_glyph, tool_button,
    tool_toggle, wordmark,
};
use viewport::{
    base_center, base_contains_point, base_size, choose_render_level, clamp_center_axis,
    level_by_index, screen_to_base, visible_tiles,
};

#[cfg(test)]
use dicom_viewer_core::{LevelIndex, SourceKind, TileDecodeBackend};
#[cfg(test)]
use eframe::egui::{pos2, vec2, ColorImage, TextureOptions};
#[cfg(test)]
use tile::{
    is_stale_job, DecodedTile, TileJobKey, TileKey, TileLoadResult, TilePriority, TileState,
};
#[cfg(test)]
use ui::fps_color;

const GPU_TILE_CACHE_LIMIT: usize = 512;
const MAX_LOADER_MESSAGES_PER_FRAME: usize = 128;
const LOADER_MESSAGE_BUDGET: Duration = Duration::from_millis(2);
const MAX_VISIBLE_UPLOADS_PER_FRAME: usize = 24;
const VISIBLE_UPLOAD_BUDGET: Duration = Duration::from_millis(6);
const MAX_PREFETCH_UPLOADS_PER_FRAME: usize = 4;
const PREFETCH_UPLOAD_BUDGET: Duration = Duration::from_millis(1);
const PREFETCH_MARGIN_TILES: i64 = 1;
const MIN_ZOOM: f32 = 0.000_001;
const MAX_ZOOM: f32 = 64.0;
const CAMERA_SMOOTHING_RESPONSE: f32 = 22.0;
const CAMERA_SMOOTHING_SNAP_PX: f32 = 0.25;
const CAMERA_SMOOTHING_SNAP_ZOOM: f32 = 0.0005;
const MIN_DISPLAY_FPS: f32 = 30.0;
const MAX_DISPLAY_FPS: f32 = 500.0;
const MAX_FALLBACK_TILE_PIXELS: u64 = 1_000_000;
const WHEEL_ZOOM_SENSITIVITY: f32 = 0.0015;
const MEASUREMENT_HIT_RADIUS: f32 = 11.0;
const MEASUREMENT_HANDLE_RADIUS: f32 = 5.5;

pub struct DicomViewerApp {
    study: Option<Arc<ViewerStudy>>,
    open_job: Option<OpenJob>,
    active_slide_id: u64,
    next_slide_id: u64,
    status: String,
    renderer: TileRenderer,
    frame_stats: FrameStats,
    center_base: Vec2,
    zoom: f32,
    fit_pending: bool,
    fit_mode: bool,
    last_canvas_size: Option<Vec2>,
    camera_motion: CameraMotion,
    show_facts_panel: bool,
    measurement: MeasurementState,
}

impl DicomViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        install_visuals(&cc.egui_ctx);
        let mut app = Self {
            study: None,
            open_job: None,
            active_slide_id: 0,
            next_slide_id: 1,
            status: "Open a WSI file or DICOM folder.".to_string(),
            renderer: TileRenderer::new(GPU_TILE_CACHE_LIMIT),
            frame_stats: FrameStats::default(),
            center_base: Vec2::ZERO,
            zoom: 1.0,
            fit_pending: false,
            fit_mode: false,
            last_canvas_size: None,
            camera_motion: CameraMotion::default(),
            show_facts_panel: false,
            measurement: MeasurementState::default(),
        };
        if let Some(path) = initial_path {
            app.start_open_path(path, &cc.egui_ctx);
        }
        app
    }

    fn start_open_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        if let Some(job) = &self.open_job {
            job.cancel();
        }
        let slide_id = self.next_slide_id;
        self.next_slide_id = self.next_slide_id.saturating_add(1);
        self.active_slide_id = slide_id;
        self.study = None;
        self.renderer.clear();
        self.clear_fit_state();
        self.camera_motion.clear();
        self.measurement.reset();
        self.status = format!("Opening {}...", path.display());

        let (sender, receiver) = mpsc::channel();
        let worker_path = path.clone();
        let repaint = ctx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let result = ViewerStudy::open_path(&worker_path).map_err(|err| err.to_string());
            if worker_cancel.load(Ordering::Relaxed) {
                return;
            }
            let sent = sender.send(OpenResult {
                slide_id,
                path: worker_path,
                result,
            });
            if sent.is_ok() {
                repaint.request_repaint();
            }
        });

        self.open_job = Some(OpenJob {
            path,
            receiver,
            cancel,
        });
        ctx.request_repaint();
    }

    fn poll_open_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.open_job else {
            return;
        };
        match job.receiver.try_recv() {
            Ok(result) => {
                let current_slide_id = self.active_slide_id;
                self.open_job = None;
                if result.slide_id != current_slide_id {
                    return;
                }
                match result.result {
                    Ok(study) => {
                        self.center_base = base_center(study.summary());
                        self.study = Some(Arc::new(study));
                        self.renderer.clear();
                        self.request_fit();
                        self.status = format!("Opened {}.", result.path.display());
                    }
                    Err(err) => {
                        self.status = format!("Failed to open {}: {err}", result.path.display());
                    }
                }
                ctx.request_repaint();
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Don't spin — poll_open_job is called every frame from logic().
            }
            Err(mpsc::TryRecvError::Disconnected) => {
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

    fn fit_to_rect(&mut self, rect: Rect, summary: &StudySummary) {
        let Some(size) = base_size(summary) else {
            return;
        };
        self.zoom = (rect.width() / size.x)
            .min(rect.height() / size.y)
            .clamp(MIN_ZOOM, MAX_ZOOM);
        self.center_base = size * 0.5;
        self.fit_pending = false;
        self.fit_mode = true;
        self.last_canvas_size = Some(rect.size());
    }

    fn clear_fit_state(&mut self) {
        self.fit_pending = false;
        self.fit_mode = false;
        self.last_canvas_size = None;
    }

    fn request_fit(&mut self) {
        self.fit_pending = true;
        self.fit_mode = true;
        self.last_canvas_size = None;
    }

    fn leave_fit_mode(&mut self) {
        self.fit_mode = false;
    }

    fn pan_by(&mut self, delta_screen: Vec2) {
        if delta_screen == Vec2::ZERO {
            return;
        }
        self.leave_fit_mode();
        self.center_base -= delta_screen / self.zoom.max(MIN_ZOOM);
    }

    fn zoom_around(&mut self, rect: Rect, pointer: egui::Pos2, factor: f32) {
        let old_zoom = self.zoom;
        let new_zoom = (old_zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (new_zoom - old_zoom).abs() < f32::EPSILON {
            return;
        }

        self.leave_fit_mode();
        let pointer_canvas = pointer - rect.min;
        let old_top_left = self.center_base - rect.size() / (2.0 * old_zoom);
        let base_under_pointer = old_top_left + pointer_canvas / old_zoom;
        let new_top_left = base_under_pointer - pointer_canvas / new_zoom;
        self.center_base = new_top_left + rect.size() / (2.0 * new_zoom);
        self.zoom = new_zoom;
    }

    fn zoom_about_center(&mut self, rect: Rect, factor: f32) {
        self.zoom_around(rect, rect.center(), factor);
    }

    fn handle_canvas_keys(&mut self, ui: &egui::Ui, rect: Rect, accepts_keys: bool) -> bool {
        if !accepts_keys {
            return false;
        }
        use egui::Key;
        let mut changed = false;
        let (zoom_in, zoom_out, fit, left, right, up, down, dt) = ui.input(|i| {
            (
                i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
                i.key_pressed(Key::Minus),
                i.key_pressed(Key::Num0),
                i.key_down(Key::ArrowLeft),
                i.key_down(Key::ArrowRight),
                i.key_down(Key::ArrowUp),
                i.key_down(Key::ArrowDown),
                i.stable_dt,
            )
        });
        if zoom_in {
            self.zoom_about_center(rect, 1.25);
            changed = true;
        }
        if zoom_out {
            self.zoom_about_center(rect, 0.8);
            changed = true;
        }
        if fit {
            self.request_fit();
            ui.ctx().request_repaint();
            changed = true;
        }
        let mut delta = Vec2::ZERO;
        let step = 720.0 * dt.clamp(1.0 / 240.0, 1.0 / 15.0);
        if left {
            delta.x += step;
        }
        if right {
            delta.x -= step;
        }
        if up {
            delta.y += step;
        }
        if down {
            delta.y -= step;
        }
        if delta != Vec2::ZERO {
            self.pan_by(delta);
            changed = true;
        }
        changed
    }

    fn clamp_view(&mut self, rect: Rect, summary: &StudySummary) {
        let mut view = self.target_view();
        clamp_camera_view(&mut view, rect, summary);
        self.center_base = view.center_base;
        self.zoom = view.zoom;
    }

    fn target_view(&self) -> CameraView {
        CameraView {
            center_base: self.center_base,
            zoom: self.zoom,
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
                request_next_frame(ui.ctx());
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
                        request_next_frame(ui.ctx());
                    }
                }
            }
        }

        interaction
    }
}

fn clamp_camera_view(view: &mut CameraView, rect: Rect, summary: &StudySummary) {
    view.zoom = view.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    let Some(size) = base_size(summary) else {
        return;
    };
    let visible_w = rect.width() / view.zoom.max(MIN_ZOOM);
    let visible_h = rect.height() / view.zoom.max(MIN_ZOOM);

    view.center_base.x = clamp_center_axis(view.center_base.x, visible_w, size.x);
    view.center_base.y = clamp_center_axis(view.center_base.y, visible_h, size.y);
}

#[derive(Debug, Clone, Copy)]
struct CameraView {
    center_base: Vec2,
    zoom: f32,
}

impl Default for CameraView {
    fn default() -> Self {
        Self {
            center_base: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

#[derive(Debug, Default)]
struct MeasurementState {
    active: bool,
    points: [Option<Vec2>; 2],
    dragging: Option<usize>,
}

impl MeasurementState {
    fn reset(&mut self) {
        self.active = false;
        self.clear_points();
    }

    fn clear_points(&mut self) {
        self.points = [None, None];
        self.dragging = None;
    }

    fn has_points(&self) -> bool {
        self.points.iter().any(Option::is_some)
    }

    fn place_next_point(&mut self, point: Vec2) {
        if self.points[0].is_none() {
            self.points[0] = Some(point);
        } else if self.points[1].is_none() {
            self.points[1] = Some(point);
        }
    }

    fn set_point(&mut self, index: usize, point: Vec2) {
        if let Some(slot) = self.points.get_mut(index) {
            *slot = Some(point);
        }
    }

    fn hit_test(&self, rect: Rect, pointer: egui::Pos2, view: CameraView) -> Option<usize> {
        self.points
            .iter()
            .enumerate()
            .filter_map(|(index, point)| {
                let point = (*point)?;
                let screen = base_to_screen(rect, point, view.center_base, view.zoom);
                let distance2 = (screen - pointer).length_sq();
                (distance2 <= MEASUREMENT_HIT_RADIUS * MEASUREMENT_HIT_RADIUS)
                    .then_some((index, distance2))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
    }

    fn distance_label(&self, summary: &StudySummary) -> Option<String> {
        let [Some(a), Some(b)] = self.points else {
            return None;
        };
        Some(format_measurement_distance(measurement_distance(
            summary, a, b,
        )))
    }
}

#[derive(Debug, Default)]
struct MeasurementInteraction {
    drag_consumed: bool,
    click_consumed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MeasurementDistance {
    Microns(f64),
    BasePixels(f64),
}

#[derive(Debug)]
struct CameraMotion {
    rendered: CameraView,
    initialized: bool,
    enabled: bool,
}

impl Default for CameraMotion {
    fn default() -> Self {
        Self {
            rendered: CameraView::default(),
            initialized: false,
            enabled: true,
        }
    }
}

impl CameraMotion {
    fn clear(&mut self) {
        self.initialized = false;
    }

    fn reset(&mut self, view: CameraView) {
        self.rendered = view;
        self.initialized = true;
    }

    fn render_view(&mut self, target: CameraView, dt: f32) -> (CameraView, bool) {
        if !self.enabled || !self.initialized {
            self.reset(target);
            return (target, false);
        }

        let alpha = camera_smoothing_alpha(dt);
        self.rendered.center_base += (target.center_base - self.rendered.center_base) * alpha;
        self.rendered.zoom = smooth_zoom(self.rendered.zoom, target.zoom, alpha);

        if camera_is_settled(self.rendered, target) {
            self.reset(target);
            return (target, false);
        }

        (self.rendered, true)
    }
}

fn camera_smoothing_alpha(dt: f32) -> f32 {
    let dt = if dt.is_finite() && dt > 0.0 {
        dt.clamp(1.0 / 240.0, 1.0 / 15.0)
    } else {
        1.0 / 60.0
    };
    (1.0 - (-CAMERA_SMOOTHING_RESPONSE * dt).exp()).clamp(0.0, 1.0)
}

fn smooth_zoom(current: f32, target: f32, alpha: f32) -> f32 {
    let current = current.clamp(MIN_ZOOM, MAX_ZOOM);
    let target = target.clamp(MIN_ZOOM, MAX_ZOOM);
    if !(current.is_finite() && target.is_finite()) {
        return target;
    }

    let current_ln = current.ln();
    let target_ln = target.ln();
    (current_ln + (target_ln - current_ln) * alpha)
        .exp()
        .clamp(MIN_ZOOM, MAX_ZOOM)
}

fn wheel_zoom_factor(scroll_y: f32) -> f32 {
    (-scroll_y * WHEEL_ZOOM_SENSITIVITY).exp()
}

fn camera_is_settled(rendered: CameraView, target: CameraView) -> bool {
    let center_screen_delta =
        (target.center_base - rendered.center_base) * target.zoom.max(MIN_ZOOM);
    let zoom_delta = (target.zoom / rendered.zoom.max(MIN_ZOOM)).ln().abs();
    center_screen_delta.length_sq() <= CAMERA_SMOOTHING_SNAP_PX * CAMERA_SMOOTHING_SNAP_PX
        && zoom_delta <= CAMERA_SMOOTHING_SNAP_ZOOM
}

impl eframe::App for DicomViewerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_dropped_files(ctx);
        self.poll_open_job(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let (stable_dt, predicted_dt) = ui.input(|input| (input.stable_dt, input.predicted_dt));
        self.frame_stats.record(stable_dt, predicted_dt);
        let mut fit_clicked = false;
        let mut zoom_out_clicked = false;
        let mut zoom_in_clicked = false;

        let study = self.study.clone();
        let opening = self.open_job.is_some();

        // ── Top toolbar ────────────────────────────────────────────
        Panel::top("toolbar")
            .exact_size(46.0)
            .frame(chrome_frame(theme::CHROME_RAISED, Margin::symmetric(12, 0)))
            .show_inside(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    wordmark(ui);
                    rule(ui);
                    if tool_button(ui, "Open file").clicked() {
                        self.pick_file(ui.ctx());
                    }
                    if tool_button(ui, "Open folder").clicked() {
                        self.pick_folder(ui.ctx());
                    }
                    rule(ui);
                    tool_toggle(ui, &mut self.show_facts_panel, "Info");
                    if study.is_some() {
                        rule(ui);
                        if tool_button(ui, "Fit").clicked() {
                            fit_clicked = true;
                        }
                        if tool_button(ui, "\u{2212}").clicked() {
                            zoom_out_clicked = true;
                        }
                        if tool_button(ui, "+").clicked() {
                            zoom_in_clicked = true;
                        }
                        let had_measurement = self.measurement.has_points();
                        if tool_toggle(ui, &mut self.measurement.active, "Measure").clicked() {
                            self.handle_measure_tool_clicked(
                                study.as_ref().map(|study| study.summary()),
                                had_measurement,
                            );
                        }
                        tool_toggle(ui, &mut self.camera_motion.enabled, "Smooth");
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        privacy_badge(ui);
                    });
                });
            });

        // ── Bottom status bar ──────────────────────────────────────
        Panel::bottom("status")
            .exact_size(28.0)
            .frame(chrome_frame(theme::CHROME, Margin::symmetric(12, 0)))
            .show_inside(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    status_glyph(ui, opening, study.is_some());
                    ui.label(
                        RichText::new(&self.status)
                            .color(theme::TEXT_MUTED)
                            .size(12.0),
                    );
                    if let Some(study) = &study {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} file{}",
                                    study.summary().file_count,
                                    if study.summary().file_count == 1 {
                                        ""
                                    } else {
                                        "s"
                                    }
                                ))
                                .color(theme::TEXT_DIM)
                                .size(12.0),
                            );
                        });
                    }
                });
            });

        // ── Left: slide facts ──────────────────────────────────────
        if self.show_facts_panel {
            Panel::left("facts")
                .resizable(true)
                .default_size(300.0)
                .size_range(252.0..=460.0)
                .frame(chrome_frame(theme::CHROME, Margin::same(0)))
                .show_inside(ui, |ui| {
                    if let Some(study) = &study {
                        facts_panel(ui, study.summary());
                    } else {
                        empty_facts(ui);
                    }
                });
        }

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

                let canvas_size = rect.size();
                let canvas_resized = self
                    .last_canvas_size
                    .is_some_and(|last| (last - canvas_size).length_sq() > 0.5);
                self.last_canvas_size = Some(canvas_size);

                if fit_clicked {
                    self.request_fit();
                }
                if self.fit_pending || (self.fit_mode && canvas_resized) {
                    self.fit_to_rect(rect, study.summary());
                }
                if zoom_out_clicked {
                    self.zoom_about_center(rect, 0.8);
                }
                if zoom_in_clicked {
                    self.zoom_about_center(rect, 1.25);
                }

                let measurement_interaction = self.handle_measurement_interaction(
                    ui,
                    &response,
                    rect,
                    study.summary(),
                    self.target_view(),
                );

                if response.dragged() && !measurement_interaction.drag_consumed {
                    self.pan_by(response.drag_delta());
                    request_next_frame(ui.ctx());
                }
                if response.double_clicked() && !measurement_interaction.click_consumed {
                    let pointer = response.interact_pointer_pos().unwrap_or(rect.center());
                    self.zoom_around(rect, pointer, 2.0);
                    request_next_frame(ui.ctx());
                }

                if response.hovered() {
                    let scroll_y = ui.input(|input| input.smooth_scroll_delta.y);
                    if scroll_y.abs() > 0.0 {
                        let pointer = ui
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(rect.center());
                        self.zoom_around(rect, pointer, wheel_zoom_factor(scroll_y));
                        request_next_frame(ui.ctx());
                    }
                    let pinch = ui.input(|input| input.zoom_delta());
                    if (pinch - 1.0).abs() > 0.001 {
                        let pointer = ui
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(rect.center());
                        self.zoom_around(rect, pointer, pinch);
                        request_next_frame(ui.ctx());
                    }
                    if ui.input(|input| input.pointer.any_down()) {
                        request_next_frame(ui.ctx());
                    }
                }

                let accepts_keys =
                    (response.hovered() || response.has_focus()) && !ui.ctx().text_edit_focused();
                if self.handle_canvas_keys(ui, rect, accepts_keys) {
                    request_next_frame(ui.ctx());
                }
                self.clamp_view(rect, study.summary());
                let target_view = self.target_view();
                let (mut render_view, animating_camera) =
                    self.camera_motion.render_view(target_view, stable_dt);
                clamp_camera_view(&mut render_view, rect, study.summary());
                if animating_camera {
                    request_next_frame(ui.ctx());
                }
                paint_slide(
                    ui.ctx(),
                    &painter,
                    rect,
                    &study,
                    self.active_slide_id,
                    render_view.center_base,
                    render_view.zoom,
                    target_view.center_base,
                    target_view.zoom,
                    animating_camera,
                    &mut self.renderer,
                );

                let hover_base = response
                    .hover_pos()
                    .map(|p| screen_to_base(rect, p, render_view.center_base, render_view.zoom));
                let tile_failure = self.renderer.tile_failure();
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

// ── Small UI building blocks ───────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn paint_slide(
    ctx: &egui::Context,
    painter: &egui::Painter,
    rect: Rect,
    study: &Arc<ViewerStudy>,
    slide_id: u64,
    render_center_base: Vec2,
    render_zoom: f32,
    target_center_base: Vec2,
    target_zoom: f32,
    target_loading_only: bool,
    renderer: &mut TileRenderer,
) {
    let summary = study.summary();
    let Some(base_size) = base_size(summary) else {
        paint_center_text(painter, rect, "slide has no levels");
        return;
    };
    let slide_rect = Rect::from_min_size(
        rect.center() + (Vec2::ZERO - render_center_base) * render_zoom,
        base_size * render_zoom,
    );
    painter.rect_filled(slide_rect, 0.0, Color32::BLACK);

    let Some(render_level) = choose_render_level(summary, render_zoom) else {
        paint_center_text(painter, rect, "slide has no renderable level");
        return;
    };
    if render_level.tile_layout.grid_size().is_none() {
        paint_center_text(
            painter,
            rect,
            "irregular tile maps are not yet rendered by the tile viewer",
        );
        return;
    }

    let render_visible = visible_tiles(
        rect,
        render_level,
        slide_id,
        render_center_base,
        render_zoom,
        0,
    );
    let render_prefetch = visible_tiles(
        rect,
        render_level,
        slide_id,
        render_center_base,
        render_zoom,
        PREFETCH_MARGIN_TILES,
    );
    let loading_level = if target_loading_only {
        choose_render_level(summary, target_zoom)
            .filter(|level| level.tile_layout.grid_size().is_some())
            .unwrap_or(render_level)
    } else {
        render_level
    };
    let loading_center_base = if target_loading_only {
        target_center_base
    } else {
        render_center_base
    };
    let loading_zoom = if target_loading_only {
        target_zoom
    } else {
        render_zoom
    };
    let loading_visible = visible_tiles(
        rect,
        loading_level,
        slide_id,
        loading_center_base,
        loading_zoom,
        0,
    );
    let loading_prefetch = if target_loading_only {
        Vec::new()
    } else {
        visible_tiles(
            rect,
            loading_level,
            slide_id,
            loading_center_base,
            loading_zoom,
            PREFETCH_MARGIN_TILES,
        )
    };
    let held_level = renderer
        .displayed_level()
        .and_then(|index| level_by_index(summary, index))
        .filter(|level| level.tile_layout.grid_size().is_some());
    let held_visible = held_level
        .map(|level| visible_tiles(rect, level, slide_id, render_center_base, render_zoom, 0))
        .unwrap_or_default();
    let fallback_levels = fallback_levels(summary, render_level, held_level);
    let fallback_visible_layers = fallback_levels
        .iter()
        .map(|level| {
            (
                *level,
                visible_tiles(rect, level, slide_id, render_center_base, render_zoom, 0),
            )
        })
        .collect::<Vec<_>>();
    let fallback_prefetch_layers = if target_loading_only {
        Vec::new()
    } else {
        fallback_levels
            .iter()
            .map(|level| {
                (
                    *level,
                    visible_tiles(
                        rect,
                        level,
                        slide_id,
                        render_center_base,
                        render_zoom,
                        PREFETCH_MARGIN_TILES,
                    ),
                )
            })
            .collect::<Vec<_>>()
    };
    let fallback_visible = flatten_tile_layers(&fallback_visible_layers);
    let fallback_prefetch = flatten_tile_layers(&fallback_prefetch_layers);

    let mut pinned = HashSet::new();
    pinned.extend(render_visible.iter().map(|tile| tile.key));
    pinned.extend(held_visible.iter().map(|tile| tile.key));
    pinned.extend(loading_visible.iter().map(|tile| tile.key));
    pinned.extend(fallback_visible.iter().map(|tile| tile.key));
    renderer.set_pinned(pinned);

    let warming_level_change = held_level.is_some_and(|level| level.index != render_level.index);
    let mut relevant_queued = HashSet::new();
    relevant_queued.extend(render_visible.iter().map(|tile| tile.key));
    relevant_queued.extend(held_visible.iter().map(|tile| tile.key));
    relevant_queued.extend(loading_visible.iter().map(|tile| tile.key));
    relevant_queued.extend(fallback_visible.iter().map(|tile| tile.key));
    if !target_loading_only && !warming_level_change {
        relevant_queued.extend(render_prefetch.iter().map(|tile| tile.key));
        relevant_queued.extend(loading_prefetch.iter().map(|tile| tile.key));
        relevant_queued.extend(fallback_prefetch.iter().map(|tile| tile.key));
    }
    renderer.retain_relevant_pending_tiles(&relevant_queued);
    renderer.poll_results(
        ctx,
        TilePollRequest {
            slide_id,
            relevant_tiles: &relevant_queued,
            visible_tiles: &loading_visible,
            fallback_tiles: &fallback_visible,
            prefetch_tiles: &loading_prefetch,
            render_level_index: loading_level.index,
        },
    );

    for (level, tiles) in &fallback_visible_layers {
        renderer.enqueue_tiles(Arc::clone(study), tiles, QueueLane::Fallback, level.index);
    }
    renderer.enqueue_tiles(
        Arc::clone(study),
        &loading_visible,
        QueueLane::Visible,
        loading_level.index,
    );
    let target_pending = renderer.pending_tile_count(&render_visible, render_level.index);
    if target_pending == 0 {
        renderer.set_displayed_level(render_level.index);
    } else if let Some(held_level) = held_level {
        renderer.set_displayed_level(held_level.index);
    } else {
        renderer.set_displayed_level(render_level.index);
    }

    if !target_loading_only
        && target_pending == 0
        && renderer.pending_tile_count(&loading_visible, loading_level.index) == 0
    {
        renderer.enqueue_tiles(
            Arc::clone(study),
            &loading_prefetch,
            QueueLane::Prefetch,
            loading_level.index,
        );
        for (level, tiles) in &fallback_prefetch_layers {
            renderer.enqueue_tiles(Arc::clone(study), tiles, QueueLane::Prefetch, level.index);
        }
    }

    // Draw available lower-resolution/held tiles first, then exact target tiles.
    // This keeps the canvas covered while target-resolution decodes catch up.
    for (level, tiles) in &fallback_visible_layers {
        for tile in tiles {
            renderer.draw_ready_tile(painter, rect, level, tile, render_center_base, render_zoom);
        }
    }
    for tile in &render_visible {
        renderer.draw_ready_tile(
            painter,
            rect,
            render_level,
            tile,
            render_center_base,
            render_zoom,
        );
    }

    // Hairline frame around the slide extent.
    painter.rect_stroke(
        slide_rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, theme::HAIRLINE),
        StrokeKind::Inside,
    );

    if renderer.loading_count() > 0 {
        request_next_frame(ctx);
    }
}

fn fallback_levels<'a>(
    summary: &'a StudySummary,
    render_level: &LevelInfo,
    held_level: Option<&'a LevelInfo>,
) -> Vec<&'a LevelInfo> {
    let mut seen = HashSet::new();
    let mut levels = Vec::new();

    for level in &summary.levels {
        if level.index == render_level.index || level.tile_layout.grid_size().is_none() {
            continue;
        }
        let is_held = held_level.is_some_and(|held| held.index == level.index);
        let is_coarser = level.downsample > render_level.downsample;
        let is_cheap_fallback = level_display_tile_pixels(level) <= MAX_FALLBACK_TILE_PIXELS;
        if (is_held || (is_coarser && is_cheap_fallback)) && seen.insert(level.index) {
            levels.push(level);
        }
    }

    levels.sort_by(|a, b| {
        b.downsample
            .total_cmp(&a.downsample)
            .then_with(|| b.index.cmp(&a.index))
    });
    levels
}

fn level_display_tile_pixels(level: &LevelInfo) -> u64 {
    let (width, height) = level.tile_layout.display_tile_size();
    u64::from(width) * u64::from(height)
}

fn flatten_tile_layers(layers: &[(&LevelInfo, Vec<VisibleTile>)]) -> Vec<VisibleTile> {
    layers
        .iter()
        .flat_map(|(_, tiles)| tiles.iter().copied())
        .collect()
}

fn paint_center_text(painter: &egui::Painter, rect: Rect, message: &str) {
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        message,
        FontId::proportional(14.0),
        theme::TEXT_MUTED,
    );
}

fn draw_measurement_overlay(
    painter: &egui::Painter,
    rect: Rect,
    summary: &StudySummary,
    measurement: &MeasurementState,
    hover_base: Option<Vec2>,
    center_base: Vec2,
    zoom: f32,
) {
    let Some(a) = measurement.points[0] else {
        return;
    };
    let a_screen = base_to_screen(rect, a, center_base, zoom);
    let b = measurement.points[1].or_else(|| {
        measurement
            .active
            .then(|| hover_base.filter(|point| base_contains_point(summary, *point)))
            .flatten()
    });
    let line_stroke = Stroke::new(2.0, theme::CYAN);

    if let Some(b) = b {
        let b_screen = base_to_screen(rect, b, center_base, zoom);
        painter.line_segment([a_screen, b_screen], line_stroke);

        if measurement.points[1].is_some() {
            if let Some(label) = measurement.distance_label(summary) {
                draw_measurement_label(painter, rect, a_screen, b_screen, &label);
            }
        }
    }

    draw_measurement_handle(painter, a_screen, measurement.dragging == Some(0));
    if let Some(b) = measurement.points[1] {
        draw_measurement_handle(
            painter,
            base_to_screen(rect, b, center_base, zoom),
            measurement.dragging == Some(1),
        );
    }
}

fn draw_measurement_label(
    painter: &egui::Painter,
    rect: Rect,
    a: egui::Pos2,
    b: egui::Pos2,
    label: &str,
) {
    let font = FontId::monospace(12.0);
    let galley = painter.layout_no_wrap(label.to_string(), font, theme::TEXT);
    let pad = egui::vec2(9.0, 5.0);
    let size = galley.size() + pad * 2.0;
    let midpoint = a + (b - a) * 0.5;
    let mut center = midpoint + egui::vec2(0.0, -20.0);
    center.x = clamp_label_axis(
        center.x,
        rect.left() + size.x * 0.5 + 8.0,
        rect.right() - size.x * 0.5 - 8.0,
    );
    center.y = clamp_label_axis(
        center.y,
        rect.top() + size.y * 0.5 + 8.0,
        rect.bottom() - size.y * 0.5 - 8.0,
    );
    let panel = Rect::from_center_size(center, size);
    painter.rect(
        panel,
        CornerRadius::same(6),
        Color32::from_rgba_unmultiplied(15, 17, 20, 232),
        Stroke::new(1.0, theme::CYAN),
        StrokeKind::Inside,
    );
    painter.galley(panel.left_top() + pad, galley, theme::TEXT);
}

fn draw_measurement_handle(painter: &egui::Painter, center: egui::Pos2, active: bool) {
    let fill = if active {
        theme::AMBER_BRIGHT
    } else {
        theme::CYAN
    };
    painter.circle_filled(center, MEASUREMENT_HANDLE_RADIUS + 2.0, theme::CANVAS_EDGE);
    painter.circle_filled(center, MEASUREMENT_HANDLE_RADIUS, fill);
    painter.circle_stroke(
        center,
        MEASUREMENT_HANDLE_RADIUS,
        Stroke::new(1.0, Color32::WHITE),
    );
}

fn measurement_ready_status(summary: Option<&StudySummary>) -> String {
    if summary.and_then(valid_mpp).is_some() {
        "Measurement active.".to_string()
    } else {
        "Measurement active; MPP unavailable, showing pixels.".to_string()
    }
}

fn measurement_distance(summary: &StudySummary, a: Vec2, b: Vec2) -> MeasurementDistance {
    let delta = b - a;
    if let Some((mpp_x, mpp_y)) = valid_mpp(summary) {
        let dx = f64::from(delta.x) * mpp_x;
        let dy = f64::from(delta.y) * mpp_y;
        MeasurementDistance::Microns(dx.hypot(dy))
    } else {
        MeasurementDistance::BasePixels(f64::from(delta.length()))
    }
}

fn valid_mpp(summary: &StudySummary) -> Option<(f64, f64)> {
    let (x, y) = summary.mpp?;
    (x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0).then_some((x, y))
}

fn format_measurement_distance(distance: MeasurementDistance) -> String {
    match distance {
        MeasurementDistance::Microns(um) if um >= 10_000.0 => format!("{:.2} mm", um / 1000.0),
        MeasurementDistance::Microns(um) if um >= 1000.0 => format!("{:.3} mm", um / 1000.0),
        MeasurementDistance::Microns(um) if um >= 100.0 => format!("{um:.0} \u{00B5}m"),
        MeasurementDistance::Microns(um) if um >= 10.0 => format!("{um:.1} \u{00B5}m"),
        MeasurementDistance::Microns(um) => format!("{um:.2} \u{00B5}m"),
        MeasurementDistance::BasePixels(px) if px >= 1000.0 => {
            format!("{} px", fmt_plain_int(px.round() as u64))
        }
        MeasurementDistance::BasePixels(px) if px >= 10.0 => format!("{px:.1} px"),
        MeasurementDistance::BasePixels(px) => format!("{px:.2} px"),
    }
}

fn fmt_plain_int(value: u64) -> String {
    let digits = value.to_string();
    let bytes = digits.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, byte) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*byte as char);
    }
    out
}

fn clamp_label_axis(value: f32, min: f32, max: f32) -> f32 {
    if min <= max {
        value.clamp(min, max)
    } else {
        (min + max) * 0.5
    }
}

fn base_to_screen(rect: Rect, base: Vec2, center_base: Vec2, zoom: f32) -> egui::Pos2 {
    rect.center() + (base - center_base) * zoom.max(MIN_ZOOM)
}

fn clamp_base_point(summary: &StudySummary, mut point: Vec2) -> Vec2 {
    let Some(size) = base_size(summary) else {
        return point;
    };
    point.x = point.x.clamp(0.0, (size.x - 1.0).max(0.0));
    point.y = point.y.clamp(0.0, (size.y - 1.0).max(0.0));
    point
}

#[derive(Debug)]
struct OpenJob {
    path: PathBuf,
    receiver: Receiver<OpenResult>,
    cancel: Arc<AtomicBool>,
}

impl OpenJob {
    fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for OpenJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Debug)]
struct OpenResult {
    slide_id: u64,
    path: PathBuf,
    result: std::result::Result<ViewerStudy, String>,
}

#[derive(Debug, Default)]
struct FrameStats {
    fps: f32,
    display_fps: f32,
}

impl FrameStats {
    fn record(&mut self, stable_dt: f32, predicted_dt: f32) {
        let Some(instant) = fps_from_dt(stable_dt) else {
            return;
        };
        self.fps = if self.fps > 0.0 {
            self.fps.mul_add(0.9, instant * 0.1)
        } else {
            instant
        };

        let display_candidate = fps_from_dt(stable_dt)
            .or_else(|| fps_from_dt(predicted_dt))
            .filter(|fps| (MIN_DISPLAY_FPS..=MAX_DISPLAY_FPS).contains(fps));
        if let Some(candidate) = display_candidate {
            self.display_fps = if self.display_fps <= 0.0 || candidate > self.display_fps {
                candidate
            } else {
                self.display_fps.mul_add(0.995, candidate * 0.005)
            };
        }
    }

    fn info(&self) -> Option<FrameRateInfo> {
        (self.fps > 0.0).then_some(FrameRateInfo {
            fps: self.fps,
            display_fps: self.display_fps.max(self.fps),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct FrameRateInfo {
    fps: f32,
    display_fps: f32,
}

fn fps_from_dt(dt: f32) -> Option<f32> {
    if dt.is_finite() && dt > 0.0 {
        Some(1.0 / dt)
    } else {
        None
    }
}

fn request_next_frame(ctx: &egui::Context) {
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::viewport::choose_display_level_index;
    use super::*;
    use dicom_viewer_core::LevelTileLayout;

    fn level(index: usize, width: u64, height: u64, downsample: f64) -> LevelInfo {
        LevelInfo {
            index: LevelIndex::from_usize(index).expect("test level index should fit in u32"),
            width,
            height,
            downsample,
            tile_layout: LevelTileLayout::Regular {
                tile_width: 256,
                tile_height: 256,
                tiles_across: width.div_ceil(256),
                tiles_down: height.div_ceil(256),
            },
        }
    }

    fn summary() -> StudySummary {
        StudySummary {
            source_path: PathBuf::from("slide.ndpi"),
            source_kind: SourceKind::File,
            format_label: "Hamamatsu WSI".into(),
            tile_decode_backend: TileDecodeBackend::Cpu,
            file_count: 1,
            dicom_instance_count: 0,
            levels: vec![
                level(0, 4096, 4096, 1.0),
                level(1, 1024, 1024, 4.0),
                level(2, 256, 256, 16.0),
            ],
            instances: Vec::new(),
            warnings: Vec::new(),
            mpp: None,
            objective_power: None,
        }
    }

    fn decoded_tile_for_test() -> DecodedTile {
        DecodedTile {
            image: ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
            width: 1,
            height: 1,
        }
    }

    #[test]
    fn zoom_selects_closest_pyramid_level() {
        let summary = summary();
        assert_eq!(
            choose_render_level(&summary, 1.0).unwrap().index,
            LevelIndex::from_u32(0)
        );
        assert_eq!(
            choose_render_level(&summary, 0.25).unwrap().index,
            LevelIndex::from_u32(1)
        );
        assert_eq!(
            choose_render_level(&summary, 0.0625).unwrap().index,
            LevelIndex::from_u32(2)
        );
    }

    #[test]
    fn render_level_does_not_upscale_lower_resolution_tiles() {
        let summary = summary();
        assert_eq!(
            choose_render_level(&summary, 0.40).unwrap().index,
            LevelIndex::from_u32(0)
        );
        assert_eq!(
            choose_render_level(&summary, 0.25).unwrap().index,
            LevelIndex::from_u32(1)
        );
        assert_eq!(
            choose_render_level(&summary, 0.10).unwrap().index,
            LevelIndex::from_u32(1)
        );
        assert_eq!(
            choose_render_level(&summary, 0.0625).unwrap().index,
            LevelIndex::from_u32(2)
        );
    }

    #[test]
    fn display_level_holds_ready_level_until_target_is_ready() {
        let ctx = egui::Context::default();
        let summary = summary();
        let mut renderer = TileRenderer::new_for_tests(8);
        let held = TileKey::for_test(1, 1, 0, 0);
        let target = TileKey::for_test(1, 0, 0, 0);
        let held_visible = vec![VisibleTile {
            key: held,
            distance2: 0,
        }];
        let target_visible = vec![VisibleTile {
            key: target,
            distance2: 0,
        }];

        renderer.insert_ready_for_test(
            held,
            ctx.load_texture(
                "held",
                ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
                TextureOptions::NEAREST,
            ),
        );

        assert_eq!(
            choose_display_level_index(
                &renderer,
                Some(&summary.levels[1]),
                &held_visible,
                &summary.levels[0],
                &target_visible,
            ),
            LevelIndex::from_u32(1)
        );

        renderer.insert_ready_for_test(
            target,
            ctx.load_texture(
                "target",
                ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
                TextureOptions::NEAREST,
            ),
        );

        assert_eq!(
            choose_display_level_index(
                &renderer,
                Some(&summary.levels[1]),
                &held_visible,
                &summary.levels[0],
                &target_visible,
            ),
            LevelIndex::from_u32(0)
        );
    }

    #[test]
    fn every_pyramid_level_is_reachable_by_zoom() {
        // Each level in a power-of-two pyramid must win selection for some
        // zoom, otherwise zooming would skip past it and it would "never
        // display". Sweep a wide zoom range and confirm full coverage.
        let summary = summary();
        let mut reached = std::collections::HashSet::new();
        let mut zoom = MIN_ZOOM;
        while zoom <= MAX_ZOOM {
            if let Some(level) = choose_render_level(&summary, zoom) {
                reached.insert(level.index);
            }
            zoom *= 1.05;
        }
        for level in &summary.levels {
            assert!(
                reached.contains(&level.index),
                "level {} is never selected across the zoom range",
                level.index
            );
        }
    }

    #[test]
    fn visible_tiles_cover_view_and_margin() {
        let summary = summary();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
        let tiles = visible_tiles(rect, &summary.levels[0], 7, vec2(512.0, 512.0), 1.0, 0);
        assert_eq!(tiles.len(), 4);
        assert!(tiles
            .iter()
            .any(|tile| tile.key.coord.col() == 1 && tile.key.coord.row() == 1));

        let with_margin = visible_tiles(
            rect,
            &summary.levels[0],
            7,
            vec2(512.0, 512.0),
            1.0,
            PREFETCH_MARGIN_TILES,
        );
        assert!(with_margin.len() > tiles.len());
    }

    #[test]
    fn priority_orders_visible_then_prefetch() {
        let visible = TilePriority {
            lane: QueueLane::Visible.priority_lane(),
            distance2: 0,
            sequence: 1,
        };
        let fallback = TilePriority {
            lane: QueueLane::Fallback.priority_lane(),
            distance2: 0,
            sequence: 0,
        };
        let prefetch = TilePriority {
            lane: QueueLane::Prefetch.priority_lane(),
            distance2: 0,
            sequence: 0,
        };
        assert!(fallback < visible);
        assert!(visible < prefetch);
        assert!(fallback < prefetch);
    }

    #[test]
    fn fallback_levels_include_coarser_levels_and_held_level() {
        let summary = summary();
        let levels = fallback_levels(&summary, &summary.levels[1], Some(&summary.levels[0]));
        let indexes = levels.iter().map(|level| level.index).collect::<Vec<_>>();

        assert_eq!(
            indexes,
            vec![LevelIndex::from_u32(2), LevelIndex::from_u32(0)]
        );
    }

    #[test]
    fn fallback_levels_skip_target_and_irregular_levels() {
        let mut summary = summary();
        summary.levels.push(LevelInfo {
            index: LevelIndex::from_u32(3),
            width: 128,
            height: 128,
            downsample: 32.0,
            tile_layout: LevelTileLayout::Irregular {
                tile_advance: (64.0, 64.0),
                tile_count: 4,
            },
        });

        let levels = fallback_levels(&summary, &summary.levels[1], None);
        let indexes = levels.iter().map(|level| level.index).collect::<Vec<_>>();

        assert_eq!(indexes, vec![LevelIndex::from_u32(2)]);
    }

    #[test]
    fn fallback_levels_skip_oversized_coarser_tiles() {
        let mut summary = summary();
        summary.levels.push(LevelInfo {
            index: LevelIndex::from_u32(3),
            width: 2048,
            height: 2048,
            downsample: 32.0,
            tile_layout: LevelTileLayout::Regular {
                tile_width: 2048,
                tile_height: 2048,
                tiles_across: 1,
                tiles_down: 1,
            },
        });

        let levels = fallback_levels(&summary, &summary.levels[1], None);
        let indexes = levels.iter().map(|level| level.index).collect::<Vec<_>>();

        assert_eq!(indexes, vec![LevelIndex::from_u32(2)]);
    }

    #[test]
    fn frame_stats_tracks_display_cadence() {
        let mut stats = FrameStats::default();
        stats.record(1.0 / 120.0, 1.0 / 60.0);
        stats.record(1.0 / 120.0, 1.0 / 60.0);

        let info = stats.info().expect("fps should be recorded");
        assert!((info.fps - 120.0).abs() < 0.01, "fps={}", info.fps);
        assert!(
            info.display_fps >= 119.0,
            "display_fps={}",
            info.display_fps
        );
    }

    #[test]
    fn frame_stats_tracks_60hz_without_penalty() {
        let mut stats = FrameStats::default();
        stats.record(1.0 / 60.0, 1.0 / 60.0);
        stats.record(1.0 / 60.0, 1.0 / 60.0);

        let info = stats.info().expect("fps should be recorded");
        assert!((info.fps - 60.0).abs() < 0.01, "fps={}", info.fps);
        assert_eq!(fps_color(info), theme::GREEN);
    }

    #[test]
    fn camera_motion_interpolates_toward_target() {
        let mut motion = CameraMotion::default();
        let start = CameraView {
            center_base: vec2(0.0, 0.0),
            zoom: 1.0,
        };
        let target = CameraView {
            center_base: vec2(100.0, 50.0),
            zoom: 4.0,
        };
        motion.reset(start);

        let (view, animating) = motion.render_view(target, 1.0 / 60.0);

        assert!(animating);
        assert!(view.center_base.x > start.center_base.x);
        assert!(view.center_base.x < target.center_base.x);
        assert!(view.center_base.y > start.center_base.y);
        assert!(view.center_base.y < target.center_base.y);
        assert!(view.zoom > start.zoom);
        assert!(view.zoom < target.zoom);
    }

    #[test]
    fn disabled_camera_motion_uses_target_view_immediately() {
        let mut motion = CameraMotion {
            enabled: false,
            ..CameraMotion::default()
        };
        motion.reset(CameraView {
            center_base: vec2(0.0, 0.0),
            zoom: 1.0,
        });
        let target = CameraView {
            center_base: vec2(100.0, 50.0),
            zoom: 4.0,
        };

        let (view, animating) = motion.render_view(target, 1.0 / 60.0);

        assert!(!animating);
        assert_eq!(view.center_base, target.center_base);
        assert_eq!(view.zoom, target.zoom);
    }

    #[test]
    fn wheel_zoom_direction_is_inverted_for_natural_scroll() {
        assert!(wheel_zoom_factor(120.0) < 1.0);
        assert!(wheel_zoom_factor(-120.0) > 1.0);
        assert_eq!(wheel_zoom_factor(0.0), 1.0);
    }

    #[test]
    fn measurement_distance_uses_mpp_axes() {
        let mut summary = summary();
        summary.mpp = Some((0.25, 0.5));

        let distance = measurement_distance(&summary, vec2(10.0, 20.0), vec2(26.0, 26.0));

        assert_eq!(distance, MeasurementDistance::Microns(4.0_f64.hypot(3.0)));
        assert_eq!(format_measurement_distance(distance), "5.00 \u{00B5}m");
    }

    #[test]
    fn measurement_distance_falls_back_to_base_pixels_without_mpp() {
        let summary = summary();

        let distance = measurement_distance(&summary, vec2(10.0, 20.0), vec2(13.0, 24.0));

        assert_eq!(distance, MeasurementDistance::BasePixels(5.0));
        assert_eq!(format_measurement_distance(distance), "5.00 px");
    }

    #[test]
    fn measurement_places_two_points_and_clear_keeps_tool_active() {
        let mut measurement = MeasurementState {
            active: true,
            ..MeasurementState::default()
        };

        measurement.place_next_point(vec2(1.0, 2.0));
        measurement.place_next_point(vec2(3.0, 4.0));
        measurement.place_next_point(vec2(5.0, 6.0));

        assert_eq!(
            measurement.points,
            [Some(vec2(1.0, 2.0)), Some(vec2(3.0, 4.0))]
        );
        assert!(measurement.has_points());

        measurement.clear_points();

        assert!(measurement.active);
        assert_eq!(measurement.points, [None, None]);
        assert!(!measurement.has_points());
    }

    #[test]
    fn fps_color_is_relative_to_estimated_display_rate() {
        assert_eq!(
            fps_color(FrameRateInfo {
                fps: 108.0,
                display_fps: 120.0,
            }),
            theme::GREEN
        );
        assert_eq!(
            fps_color(FrameRateInfo {
                fps: 95.0,
                display_fps: 120.0,
            }),
            theme::WARN
        );
        assert_eq!(
            fps_color(FrameRateInfo {
                fps: 70.0,
                display_fps: 120.0,
            }),
            theme::TEXT_MUTED
        );
    }

    #[test]
    fn tile_jobs_are_scoped_to_active_slide() {
        let key = TileJobKey {
            slide_id: 3,
            tile: TileKey::for_test(3, 0, 0, 0),
        };
        assert!(!is_stale_job(key, 3));
        assert!(is_stale_job(key, 2));
    }

    #[test]
    fn finished_tiles_are_kept_for_active_slide() {
        let ctx = egui::Context::default();
        let mut renderer = TileRenderer::new_for_tests(4);
        let tile = TileKey::for_test(1, 0, 2, 3);
        renderer.cache.insert(tile, TileState::Decoding);
        renderer.loading_count = 1;
        let relevant = HashSet::from([tile]);

        renderer.stage_finished_tile(
            1,
            &relevant,
            TileLoadResult {
                key: TileJobKey { slide_id: 1, tile },
                result: Ok(decoded_tile_for_test()),
            },
        );

        assert!(matches!(
            renderer.cache.get(&tile),
            Some(TileState::Decoded { .. })
        ));
        assert_eq!(renderer.loading_count, 1);

        let uploaded = renderer.upload_pending_tile(&ctx, tile);
        assert!(uploaded);
        assert!(matches!(
            renderer.cache.get(&tile),
            Some(TileState::Ready { .. })
        ));
        assert_eq!(renderer.loading_count, 0);
    }

    #[test]
    fn pruning_keeps_inflight_decodes_but_drops_queued_work() {
        let mut renderer = TileRenderer::new_for_tests(4);
        let queued = TileKey::for_test(1, 0, 1, 1);
        let decoding = TileKey::for_test(1, 0, 2, 2);
        renderer.cache.insert(queued, TileState::Queued);
        renderer.cache.insert(decoding, TileState::Decoding);
        renderer.loading_count = 2;

        renderer.retain_relevant_pending_tiles(&HashSet::new());

        assert!(!renderer.cache.contains_key(&queued));
        assert!(matches!(
            renderer.cache.get(&decoding),
            Some(TileState::Decoding)
        ));
        assert_eq!(renderer.loading_count, 1);
    }

    #[test]
    fn pending_tile_count_tracks_only_missing_queued_and_decoding_tiles() {
        let ctx = egui::Context::default();
        let texture = ctx.load_texture(
            "ready",
            ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
            TextureOptions::NEAREST,
        );
        let mut renderer = TileRenderer::new_for_tests(4);
        let missing = TileKey::for_test(1, 0, 0, 0);
        let queued = TileKey::for_test(1, 0, 1, 0);
        let decoding = TileKey::for_test(1, 0, 2, 0);
        let ready = TileKey::for_test(1, 0, 3, 0);
        let failed = TileKey::for_test(1, 0, 4, 0);
        let decoded = TileKey::for_test(1, 0, 5, 0);
        renderer.cache.insert(queued, TileState::Queued);
        renderer.cache.insert(decoding, TileState::Decoding);
        renderer.cache.insert(
            ready,
            TileState::Ready {
                texture,
                width: 1,
                height: 1,
            },
        );
        renderer.cache.insert(failed, TileState::Failed);
        renderer.cache.insert(
            decoded,
            TileState::Decoded {
                tile: decoded_tile_for_test(),
            },
        );

        let tiles = [missing, queued, decoding, ready, failed, decoded]
            .into_iter()
            .map(|key| VisibleTile { key, distance2: 0 })
            .collect::<Vec<_>>();

        assert_eq!(
            renderer.pending_tile_count(&tiles, LevelIndex::from_u32(0)),
            4
        );
    }

    #[test]
    fn cache_eviction_keeps_pinned_tiles() {
        let ctx = egui::Context::default();
        let texture = |name: &str| {
            ctx.load_texture(
                name,
                ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
                TextureOptions::NEAREST,
            )
        };
        let mut renderer = TileRenderer::new_for_tests(2);
        let a = TileKey::for_test(1, 0, 0, 0);
        let b = TileKey::for_test(1, 0, 1, 0);
        let c = TileKey::for_test(1, 0, 2, 0);
        renderer.set_pinned(HashSet::from([a]));
        renderer.insert_ready_for_test(a, texture("a"));
        renderer.insert_ready_for_test(b, texture("b"));
        renderer.insert_ready_for_test(c, texture("c"));

        assert!(matches!(
            renderer.cache.get(&a),
            Some(TileState::Ready { .. })
        ));
        assert!(matches!(
            renderer.cache.get(&c),
            Some(TileState::Ready { .. })
        ));
        assert_eq!(renderer.ready_count, 2);
    }

    #[test]
    fn lru_touch_keeps_one_entry_per_tile() {
        let mut renderer = TileRenderer::new_for_tests(4);
        let tile = TileKey::for_test(1, 0, 0, 0);

        renderer.touch(tile);
        renderer.touch(tile);
        renderer.touch(tile);

        assert_eq!(renderer.lru.len(), 1);
        assert_eq!(renderer.lru.front(), Some(&tile));
    }
}
