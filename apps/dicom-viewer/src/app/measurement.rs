use dicom_viewer_core::StudySummary;
use eframe::egui::{self, Color32, CornerRadius, FontId, Rect, Stroke, StrokeKind, Vec2};

use super::camera::{CameraView, MIN_ZOOM};
use super::format::format_integer;
use super::theme;
use super::viewport::{base_contains_point, base_size};

const MEASUREMENT_HIT_RADIUS: f32 = 11.0;
const MEASUREMENT_HANDLE_RADIUS: f32 = 5.5;

#[derive(Debug, Default)]
pub(super) struct MeasurementState {
    pub(super) active: bool,
    pub(super) points: [Option<Vec2>; 2],
    pub(super) dragging: Option<usize>,
}

impl MeasurementState {
    pub(super) fn reset(&mut self) {
        self.active = false;
        self.clear_points();
    }

    pub(super) fn clear_points(&mut self) {
        self.points = [None, None];
        self.dragging = None;
    }

    pub(super) fn has_points(&self) -> bool {
        self.points.iter().any(Option::is_some)
    }

    pub(super) fn place_next_point(&mut self, point: Vec2) {
        if self.points[0].is_none() {
            self.points[0] = Some(point);
        } else if self.points[1].is_none() {
            self.points[1] = Some(point);
        }
    }

    pub(super) fn set_point(&mut self, index: usize, point: Vec2) {
        if let Some(slot) = self.points.get_mut(index) {
            *slot = Some(point);
        }
    }

    pub(super) fn hit_test(
        &self,
        rect: Rect,
        pointer: egui::Pos2,
        view: CameraView,
    ) -> Option<usize> {
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

    pub(super) fn distance_label(&self, summary: &StudySummary) -> Option<String> {
        let [Some(a), Some(b)] = self.points else {
            return None;
        };
        Some(format_measurement_distance(measurement_distance(
            summary, a, b,
        )))
    }
}

#[derive(Debug, Default)]
pub(super) struct MeasurementInteraction {
    pub(super) drag_consumed: bool,
    pub(super) click_consumed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum MeasurementDistance {
    Microns(f64),
    BasePixels(f64),
}
pub(super) fn draw_measurement_overlay(
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

pub(super) fn measurement_ready_status(summary: Option<&StudySummary>) -> String {
    if summary.and_then(valid_mpp).is_some() {
        "Measurement active.".to_string()
    } else {
        "Measurement active; MPP unavailable, showing pixels.".to_string()
    }
}

pub(super) fn measurement_distance(
    summary: &StudySummary,
    a: Vec2,
    b: Vec2,
) -> MeasurementDistance {
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

pub(super) fn format_measurement_distance(distance: MeasurementDistance) -> String {
    match distance {
        MeasurementDistance::Microns(um) if um >= 10_000.0 => format!("{:.2} mm", um / 1000.0),
        MeasurementDistance::Microns(um) if um >= 1000.0 => format!("{:.3} mm", um / 1000.0),
        MeasurementDistance::Microns(um) if um >= 100.0 => format!("{um:.0} \u{00B5}m"),
        MeasurementDistance::Microns(um) if um >= 10.0 => format!("{um:.1} \u{00B5}m"),
        MeasurementDistance::Microns(um) => format!("{um:.2} \u{00B5}m"),
        MeasurementDistance::BasePixels(px) if px >= 1000.0 => {
            format!("{} px", format_integer(px.round() as u64))
        }
        MeasurementDistance::BasePixels(px) if px >= 10.0 => format!("{px:.1} px"),
        MeasurementDistance::BasePixels(px) => format!("{px:.2} px"),
    }
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

pub(super) fn clamp_base_point(summary: &StudySummary, mut point: Vec2) -> Vec2 {
    let Some(size) = base_size(summary) else {
        return point;
    };
    point.x = point.x.clamp(0.0, (size.x - 1.0).max(0.0));
    point.y = point.y.clamp(0.0, (size.y - 1.0).max(0.0));
    point
}
