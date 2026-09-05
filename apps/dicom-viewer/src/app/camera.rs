use dicom_viewer_core::StudySummary;
use eframe::egui::{self, Rect, Vec2};

use super::viewport::{base_size, clamp_center_axis};

pub(super) const MIN_ZOOM: f32 = 0.000_001;
pub(super) const MAX_ZOOM: f32 = 64.0;
const MIN_FIT_ZOOM_FACTOR: f32 = 0.9;
const CAMERA_SMOOTHING_RESPONSE: f32 = 22.0;
const CAMERA_SMOOTHING_SNAP_PX: f32 = 0.25;
const CAMERA_SMOOTHING_SNAP_ZOOM: f32 = 0.0005;
const WHEEL_ZOOM_SENSITIVITY: f32 = 0.0015;
const MIN_WHEEL_ZOOM_SPEED: f32 = 0.25;
const MAX_WHEEL_ZOOM_SPEED: f32 = 4.0;
const MAX_WHEEL_ZOOM_EXPONENT: f32 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(super) struct WheelZoomSettings {
    speed: f32,
    inverted: bool,
}

impl WheelZoomSettings {
    pub(super) fn for_os(os: &str) -> Self {
        if os == "windows" {
            Self::new(2.0, false)
        } else {
            Self::new(1.0, true)
        }
    }

    pub(super) fn new(speed: f32, inverted: bool) -> Self {
        Self { speed, inverted }.sanitized()
    }

    pub(super) fn speed(self) -> f32 {
        self.speed
    }

    pub(super) fn speed_mut(&mut self) -> &mut f32 {
        &mut self.speed
    }

    pub(super) fn inverted(self) -> bool {
        self.inverted
    }

    pub(super) fn inverted_mut(&mut self) -> &mut bool {
        &mut self.inverted
    }

    pub(super) fn sanitized(mut self) -> Self {
        if !self.speed.is_finite() {
            self.speed = 1.0;
        }
        self.speed = self.speed.clamp(MIN_WHEEL_ZOOM_SPEED, MAX_WHEEL_ZOOM_SPEED);
        self
    }
}

impl Default for WheelZoomSettings {
    fn default() -> Self {
        Self::for_os(std::env::consts::OS)
    }
}

fn clamp_camera_view_to_min(view: &mut CameraView, summary: &StudySummary, minimum_zoom: f32) {
    view.zoom = view.zoom.clamp(minimum_zoom, MAX_ZOOM);
    let Some(size) = base_size(summary) else {
        return;
    };

    view.center_base.x = clamp_center_axis(view.center_base.x, size.x);
    view.center_base.y = clamp_center_axis(view.center_base.y, size.y);
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CameraView {
    pub(super) center_base: Vec2,
    pub(super) zoom: f32,
}

impl CameraView {
    pub(super) fn base_to_screen(self, rect: Rect, base: Vec2) -> egui::Pos2 {
        rect.center() + (base - self.center_base) * self.zoom.max(MIN_ZOOM)
    }
}

impl Default for CameraView {
    fn default() -> Self {
        Self {
            center_base: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CameraFrame {
    pub(super) rendered: CameraView,
    pub(super) target: CameraView,
    pub(super) animating: bool,
}

#[derive(Debug)]
pub(super) struct CameraMotion {
    pub(super) rendered: CameraView,
    pub(super) initialized: bool,
    pub(super) enabled: bool,
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
    pub(super) fn clear(&mut self) {
        self.initialized = false;
    }

    pub(super) fn reset(&mut self, view: CameraView) {
        self.rendered = view;
        self.initialized = true;
    }

    pub(super) fn render_view(&mut self, target: CameraView, dt: f32) -> (CameraView, bool) {
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

#[derive(Debug)]
pub(super) struct CameraState {
    target: CameraView,
    motion: CameraMotion,
    fit_pending: bool,
    fit_mode: bool,
    last_canvas_size: Option<Vec2>,
    minimum_zoom: f32,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            target: CameraView::default(),
            motion: CameraMotion::default(),
            fit_pending: false,
            fit_mode: false,
            last_canvas_size: None,
            minimum_zoom: MIN_ZOOM,
        }
    }
}

impl CameraState {
    pub(super) fn clear_for_open(&mut self) {
        self.target = CameraView::default();
        self.fit_pending = false;
        self.fit_mode = false;
        self.last_canvas_size = None;
        self.minimum_zoom = MIN_ZOOM;
        self.motion.clear();
    }

    pub(super) fn reset_for_study(&mut self, summary: &StudySummary) {
        self.target = CameraView {
            center_base: super::viewport::base_center(summary),
            zoom: 1.0,
        };
        self.motion.clear();
        self.request_fit();
    }

    pub(super) fn smoothing_enabled_mut(&mut self) -> &mut bool {
        &mut self.motion.enabled
    }

    pub(super) fn request_fit(&mut self) {
        self.fit_pending = true;
        self.fit_mode = true;
        self.last_canvas_size = None;
    }

    pub(super) fn center_on_base_bounds(&mut self, bounds: [f64; 4]) {
        let center = Vec2::new(
            ((bounds[0] + bounds[2]) * 0.5) as f32,
            ((bounds[1] + bounds[3]) * 0.5) as f32,
        );
        if center.is_finite() {
            self.leave_fit_mode();
            self.target.center_base = center;
        }
    }

    pub(super) fn prepare_canvas(&mut self, rect: Rect, summary: &StudySummary) {
        self.minimum_zoom = minimum_zoom_for_rect(rect, summary);
        let canvas_size = rect.size();
        let resized = self
            .last_canvas_size
            .is_some_and(|last| (last - canvas_size).length_sq() > 0.5);
        self.last_canvas_size = Some(canvas_size);
        if self.fit_pending || (self.fit_mode && resized) {
            self.fit_to_rect(rect, summary);
        }
        clamp_camera_view_to_min(&mut self.target, summary, self.minimum_zoom);
    }

    pub(super) fn target_view(&self) -> CameraView {
        self.target
    }

    pub(super) fn render_view(
        &mut self,
        _rect: Rect,
        summary: &StudySummary,
        dt: f32,
    ) -> (CameraView, bool) {
        clamp_camera_view_to_min(&mut self.target, summary, self.minimum_zoom);
        let (mut rendered, animating) = self.motion.render_view(self.target, dt);
        clamp_camera_view_to_min(&mut rendered, summary, self.minimum_zoom);
        (rendered, animating)
    }

    pub(super) fn frame(&mut self, rect: Rect, summary: &StudySummary, dt: f32) -> CameraFrame {
        let (rendered, animating) = self.render_view(rect, summary, dt);
        CameraFrame {
            rendered,
            target: self.target,
            animating,
        }
    }

    pub(super) fn retarget_frame(
        &mut self,
        rendered: CameraView,
        summary: &StudySummary,
    ) -> CameraFrame {
        clamp_camera_view_to_min(&mut self.target, summary, self.minimum_zoom);
        if !self.motion.enabled || camera_is_settled(rendered, self.target) {
            self.motion.reset(self.target);
            return CameraFrame {
                rendered: self.target,
                target: self.target,
                animating: false,
            };
        }

        CameraFrame {
            rendered,
            target: self.target,
            animating: true,
        }
    }

    pub(super) fn pan_by(&mut self, delta_screen: Vec2) {
        if delta_screen == Vec2::ZERO {
            return;
        }
        self.leave_fit_mode();
        self.target.center_base -= delta_screen / self.target.zoom.max(MIN_ZOOM);
    }

    pub(super) fn pan_by_rendered(&mut self, delta_screen: Vec2, rendered: CameraView) {
        if delta_screen == Vec2::ZERO {
            return;
        }
        self.leave_fit_mode();
        self.target.center_base -= delta_screen / rendered.zoom.max(MIN_ZOOM);
    }

    pub(super) fn zoom_around(&mut self, rect: Rect, pointer: egui::Pos2, factor: f32) {
        let old_zoom = self.target.zoom;
        let new_zoom = (old_zoom * factor).clamp(self.minimum_zoom, MAX_ZOOM);
        if (new_zoom - old_zoom).abs() < f32::EPSILON {
            return;
        }

        self.leave_fit_mode();
        let pointer_canvas = pointer - rect.min;
        let old_top_left = self.target.center_base - rect.size() / (2.0 * old_zoom);
        let base_under_pointer = old_top_left + pointer_canvas / old_zoom;
        let new_top_left = base_under_pointer - pointer_canvas / new_zoom;
        self.target.center_base = new_top_left + rect.size() / (2.0 * new_zoom);
        self.target.zoom = new_zoom;
    }

    pub(super) fn zoom_around_rendered(
        &mut self,
        rect: Rect,
        pointer: egui::Pos2,
        factor: f32,
        rendered: CameraView,
    ) {
        let new_zoom = (self.target.zoom * factor).clamp(self.minimum_zoom, MAX_ZOOM);
        if (new_zoom - self.target.zoom).abs() < f32::EPSILON {
            return;
        }

        self.leave_fit_mode();
        let base_under_pointer =
            rendered.center_base + (pointer - rect.center()) / rendered.zoom.max(MIN_ZOOM);
        self.target.center_base = base_under_pointer - (pointer - rect.center()) / new_zoom;
        self.target.zoom = new_zoom;
    }

    pub(super) fn zoom_about_center(&mut self, rect: Rect, factor: f32) {
        self.zoom_around(rect, rect.center(), factor);
    }

    pub(super) fn handle_keys(&mut self, ui: &egui::Ui, rect: Rect, accepts_keys: bool) -> bool {
        if !accepts_keys {
            return false;
        }
        use egui::Key;
        let mut changed = false;
        let (zoom_in, zoom_out, fit, left, right, up, down, dt) = ui.input(|input| {
            (
                input.key_pressed(Key::Plus) || input.key_pressed(Key::Equals),
                input.key_pressed(Key::Minus),
                input.key_pressed(Key::Num0),
                input.key_down(Key::ArrowLeft),
                input.key_down(Key::ArrowRight),
                input.key_down(Key::ArrowUp),
                input.key_down(Key::ArrowDown),
                input.stable_dt,
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

    fn fit_to_rect(&mut self, rect: Rect, summary: &StudySummary) {
        let Some(size) = base_size(summary) else {
            return;
        };
        self.target.zoom = fit_zoom(rect, size);
        self.target.center_base = size * 0.5;
        self.fit_pending = false;
        self.fit_mode = true;
        self.last_canvas_size = Some(rect.size());
    }

    fn leave_fit_mode(&mut self) {
        self.fit_mode = false;
    }
}

fn fit_zoom(rect: Rect, size: Vec2) -> f32 {
    (rect.width() / size.x)
        .min(rect.height() / size.y)
        .clamp(MIN_ZOOM, MAX_ZOOM)
}

fn minimum_zoom_for_rect(rect: Rect, summary: &StudySummary) -> f32 {
    base_size(summary)
        .map(|size| (fit_zoom(rect, size) * MIN_FIT_ZOOM_FACTOR).clamp(MIN_ZOOM, MAX_ZOOM))
        .unwrap_or(MIN_ZOOM)
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

pub(super) fn wheel_zoom_factor(scroll_y: f32, settings: WheelZoomSettings) -> f32 {
    let direction = if settings.inverted() { -1.0 } else { 1.0 };
    (scroll_y * WHEEL_ZOOM_SENSITIVITY * settings.speed() * direction)
        .clamp(-MAX_WHEEL_ZOOM_EXPONENT, MAX_WHEEL_ZOOM_EXPONENT)
        .exp()
}

pub(super) fn raw_wheel_delta_y(input: &egui::InputState) -> f32 {
    let line_scroll_speed = egui::InputOptions::default().line_scroll_speed;
    input
        .events
        .iter()
        .filter_map(|event| match event {
            egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } if !modifiers.command => Some(match unit {
                egui::MouseWheelUnit::Point => delta.y,
                egui::MouseWheelUnit::Line => delta.y * line_scroll_speed,
                egui::MouseWheelUnit::Page => delta.y * input.viewport_rect().height(),
            }),
            _ => None,
        })
        .sum()
}

fn camera_is_settled(rendered: CameraView, target: CameraView) -> bool {
    let center_screen_delta =
        (target.center_base - rendered.center_base) * target.zoom.max(MIN_ZOOM);
    let zoom_delta = (target.zoom / rendered.zoom.max(MIN_ZOOM)).ln().abs();
    center_screen_delta.length_sq() <= CAMERA_SMOOTHING_SNAP_PX * CAMERA_SMOOTHING_SNAP_PX
        && zoom_delta <= CAMERA_SMOOTHING_SNAP_ZOOM
}
