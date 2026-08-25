use dicom_viewer_core::StudySummary;
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Stroke, StrokeKind, Vec2,
};

use super::super::{
    camera::MIN_ZOOM, format::format_integer, theme, tile::TileFailureInfo,
    viewport::base_contains_point,
};

const MIN_DISPLAY_FPS: f32 = 30.0;
const MAX_DISPLAY_FPS: f32 = 500.0;

#[derive(Debug, Default)]
pub(in crate::app) struct FrameStats {
    fps: f32,
    display_fps: f32,
}

impl FrameStats {
    pub(in crate::app) fn record(&mut self, stable_dt: f32, predicted_dt: f32) {
        let Some(instant) = fps_from_dt(stable_dt) else {
            return;
        };
        self.fps = if self.fps > 0.0 {
            self.fps.mul_add(0.9, instant * 0.1)
        } else {
            instant
        };

        let display_candidate = fps_from_dt(predicted_dt)
            .filter(|fps| (MIN_DISPLAY_FPS..=MAX_DISPLAY_FPS).contains(fps));
        if let Some(candidate) = display_candidate {
            self.display_fps = if self.display_fps <= 0.0 || candidate > self.display_fps {
                candidate
            } else {
                self.display_fps.mul_add(0.995, candidate * 0.005)
            };
        }
    }

    pub(in crate::app) fn info(&self) -> Option<FrameRateInfo> {
        (self.fps > 0.0).then_some(FrameRateInfo {
            fps: self.fps,
            display_fps: self.display_fps.max(self.fps),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::app) struct FrameRateInfo {
    pub(in crate::app) fps: f32,
    pub(in crate::app) display_fps: f32,
}

fn fps_from_dt(dt: f32) -> Option<f32> {
    if dt.is_finite() && dt > 0.0 {
        Some(1.0 / dt)
    } else {
        None
    }
}

pub(in crate::app) fn paint_canvas_background(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, theme::CANVAS);
    let stroke = Stroke::new(1.0, theme::HAIRLINE_SOFT);
    let len = 14.0;
    let margin = 10.0;
    let corners = [
        (rect.left_top(), vec2(1.0, 0.0), vec2(0.0, 1.0)),
        (rect.right_top(), vec2(-1.0, 0.0), vec2(0.0, 1.0)),
        (rect.left_bottom(), vec2(1.0, 0.0), vec2(0.0, -1.0)),
        (rect.right_bottom(), vec2(-1.0, 0.0), vec2(0.0, -1.0)),
    ];
    for (corner, hx, vy) in corners {
        let anchor = corner + hx * margin + vy * margin;
        painter.line_segment([anchor, anchor + hx * len], stroke);
        painter.line_segment([anchor, anchor + vy * len], stroke);
    }
}

pub(in crate::app) fn paint_empty_state(painter: &egui::Painter, rect: Rect, opening: bool) {
    let cx = rect.center().x;
    let base_y = rect.center().y;
    let tiers = [
        (132.0_f32, theme::HAIRLINE),
        (96.0, theme::TEXT_DIM),
        (60.0, theme::AMBER),
        (28.0, theme::CYAN),
    ];
    for (i, (width, color)) in tiers.iter().enumerate() {
        let y = base_y - i as f32 * 16.0;
        painter.rect_filled(
            Rect::from_center_size(pos2(cx, y), vec2(*width, 12.0)),
            CornerRadius::same(2),
            *color,
        );
    }
    let message = if opening {
        "opening\u{2026}"
    } else {
        "drop a WSI file or DICOM folder"
    };
    painter.text(
        pos2(cx, base_y + 44.0),
        Align2::CENTER_CENTER,
        message,
        FontId::proportional(14.0),
        theme::TEXT_MUTED,
    );
    if !opening {
        painter.text(
            pos2(cx, base_y + 66.0),
            Align2::CENTER_CENTER,
            "or use Open file / Open folder",
            FontId::proportional(11.5),
            theme::TEXT_DIM,
        );
    }
}

pub(in crate::app) struct OverlayInfo<'a> {
    pub(in crate::app) summary: &'a StudySummary,
    pub(in crate::app) zoom: f32,
    pub(in crate::app) frame_rate: Option<FrameRateInfo>,
    pub(in crate::app) hover_base: Option<Vec2>,
    pub(in crate::app) tile_failure: Option<&'a TileFailureInfo>,
    pub(in crate::app) debug_stats: Option<&'a str>,
}

pub(in crate::app) fn draw_canvas_overlays(
    painter: &egui::Painter,
    rect: Rect,
    info: OverlayInfo<'_>,
) {
    let font = FontId::monospace(12.0);
    let mut parts: Vec<(String, Color32)> = Vec::new();
    parts.push((fmt_zoom(info.zoom), theme::CYAN));
    if let Some(frame_rate) = info.frame_rate {
        parts.push((format!("{:.0} fps", frame_rate.fps), fps_color(frame_rate)));
    }
    if let Some(base) = info.hover_base {
        if base_contains_point(info.summary, base) {
            parts.push((
                format!("{}, {}", base.x as i64, base.y as i64),
                theme::TEXT_MUTED,
            ));
        }
    }

    let galleys: Vec<_> = parts
        .iter()
        .map(|(text, color)| painter.layout_no_wrap(text.clone(), font.clone(), *color))
        .collect();
    let gap = 16.0;
    let total_w: f32 = galleys.iter().map(|g| g.size().x).sum::<f32>()
        + gap * galleys.len().saturating_sub(1) as f32;
    let pad = vec2(11.0, 6.0);
    let panel = Rect::from_min_size(
        rect.left_top() + vec2(12.0, 12.0),
        vec2(total_w + pad.x * 2.0, font.size + pad.y * 2.0),
    );
    painter.rect(
        panel,
        CornerRadius::same(6),
        Color32::from_rgba_unmultiplied(15, 17, 20, 226),
        Stroke::new(1.0, theme::HAIRLINE),
        StrokeKind::Inside,
    );
    let mut x = panel.left() + pad.x;
    let y = panel.center().y;
    for galley in &galleys {
        painter.galley(
            pos2(x, y - galley.size().y * 0.5),
            galley.clone(),
            theme::TEXT,
        );
        x += galley.size().x + gap;
    }

    draw_scale_bar(painter, rect, info.summary, info.zoom);
    if should_draw_tile_failure(info.tile_failure.is_some(), info.debug_stats.is_some()) {
        let failure = info
            .tile_failure
            .expect("failure presence was checked before drawing diagnostics");
        draw_tile_failure_badge(painter, rect, failure);
    }
    if let Some(debug_stats) = info.debug_stats {
        draw_debug_stats(painter, rect, debug_stats);
    }
}

const fn should_draw_tile_failure(failure_present: bool, diagnostics_enabled: bool) -> bool {
    failure_present && diagnostics_enabled
}

fn draw_debug_stats(painter: &egui::Painter, rect: Rect, text: &str) {
    let font = FontId::monospace(10.5);
    let galley = painter.layout(
        text.to_string(),
        font,
        theme::TEXT_MUTED,
        rect.width().mul_add(0.85, -24.0).max(120.0),
    );
    let pad = vec2(9.0, 6.0);
    let panel = Rect::from_min_size(
        rect.left_top() + vec2(12.0, 50.0),
        galley.size() + pad * 2.0,
    );
    painter.rect(
        panel,
        CornerRadius::same(5),
        Color32::from_rgba_unmultiplied(15, 17, 20, 226),
        Stroke::new(1.0, theme::HAIRLINE),
        StrokeKind::Inside,
    );
    painter.galley(panel.left_top() + pad, galley, theme::TEXT_MUTED);
}

fn draw_tile_failure_badge(painter: &egui::Painter, rect: Rect, failure: &TileFailureInfo) {
    let font = FontId::proportional(11.5);
    let latest = truncate_text(
        &failure.latest,
        ((rect.width() / 7.0) as usize).clamp(28, 150),
    );
    let text = if failure.count == 1 {
        latest
    } else {
        format!("{} tile decode errors - latest: {latest}", failure.count)
    };
    let galley = painter.layout_no_wrap(text, font, theme::WARN);
    let pad = vec2(10.0, 6.0);
    let panel = Rect::from_min_size(
        pos2(
            rect.right() - galley.size().x - pad.x * 2.0 - 12.0,
            rect.bottom() - galley.size().y - pad.y * 2.0 - 16.0,
        ),
        galley.size() + pad * 2.0,
    );
    painter.rect(
        panel,
        CornerRadius::same(6),
        Color32::from_rgba_unmultiplied(38, 28, 15, 236),
        Stroke::new(1.0, theme::WARN),
        StrokeKind::Inside,
    );
    painter.galley(panel.left_top() + pad, galley, theme::WARN);
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return text.to_string();
        };
        out.push(ch);
    }
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

pub(in crate::app) fn fps_color(frame_rate: FrameRateInfo) -> Color32 {
    let ratio = frame_rate.fps / frame_rate.display_fps.max(1.0);
    if ratio >= 0.9 {
        theme::GREEN
    } else if ratio >= 0.75 {
        theme::WARN
    } else {
        theme::TEXT_MUTED
    }
}

fn draw_scale_bar(painter: &egui::Painter, rect: Rect, summary: &StudySummary, zoom: f32) {
    let z = zoom.max(MIN_ZOOM);
    let target_px = 124.0_f32;
    let (label, bar_px) = if let Some((mpp_x, _)) = summary.mpp {
        let microns_per_screen_px = mpp_x as f32 / z;
        let nice = nice_round(target_px * microns_per_screen_px);
        let bar_px = nice / microns_per_screen_px;
        if nice >= 1000.0 {
            (format!("{:.2} mm", nice / 1000.0), bar_px)
        } else {
            (format!("{nice:.0} \u{00B5}m"), bar_px)
        }
    } else {
        let base_px_per_screen_px = 1.0 / z;
        let nice = nice_round(target_px * base_px_per_screen_px);
        let bar_px = nice / base_px_per_screen_px;
        (format!("{} px", format_integer(nice as u64)), bar_px)
    };

    let y = rect.bottom() - 22.0;
    let x0 = rect.left() + 16.0;
    let x1 = x0 + bar_px.clamp(8.0, rect.width() * 0.5);
    let stroke = Stroke::new(2.0, theme::TEXT_MUTED);
    painter.line_segment([pos2(x0, y), pos2(x1, y)], stroke);
    painter.line_segment([pos2(x0, y - 4.0), pos2(x0, y + 4.0)], stroke);
    painter.line_segment([pos2(x1, y - 4.0), pos2(x1, y + 4.0)], stroke);
    painter.text(
        pos2((x0 + x1) * 0.5, y - 7.0),
        Align2::CENTER_BOTTOM,
        label,
        FontId::monospace(11.0),
        theme::TEXT_MUTED,
    );
}

fn nice_round(value: f32) -> f32 {
    if value <= 0.0 || !value.is_finite() {
        return 1.0;
    }
    let exponent = value.log10().floor();
    let base = 10f32.powf(exponent);
    let mantissa = value / base;
    let nice = if mantissa < 1.5 {
        1.0
    } else if mantissa < 3.5 {
        2.0
    } else if mantissa < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * base
}

fn fmt_zoom(zoom: f32) -> String {
    let percent = zoom * 100.0;
    if percent >= 10.0 {
        format!("{percent:.0}%")
    } else if percent >= 1.0 {
        format!("{percent:.1}%")
    } else {
        format!("{percent:.2}%")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{run_ui, summary};

    #[test]
    fn tile_failure_details_are_visible_only_with_diagnostics_enabled() {
        assert!(!should_draw_tile_failure(true, false));
        assert!(should_draw_tile_failure(true, true));
        assert!(!should_draw_tile_failure(false, true));
    }

    #[test]
    fn frame_statistics_reject_invalid_samples_and_track_display_rate() {
        let mut stats = FrameStats::default();
        stats.record(0.0, f32::NAN);
        assert!(stats.info().is_none());

        stats.record(1.0 / 60.0, 1.0 / 120.0);
        stats.record(1.0 / 30.0, 1.0 / 60.0);
        let info = stats
            .info()
            .expect("valid samples should produce frame-rate info");
        assert!(info.fps > 50.0 && info.fps < 60.0);
        assert!(info.display_fps >= info.fps);
    }

    #[test]
    fn overlay_paints_empty_loaded_and_diagnostic_states() {
        let mut study = summary();
        study.mpp = Some((0.25, 0.25));
        let failure = TileFailureInfo {
            count: 2,
            latest: "decode failed after a deliberately long diagnostic message".into(),
        };
        let output = run_ui(|ui| {
            let rect = ui.available_rect_before_wrap();
            let painter = ui.painter_at(rect);
            paint_canvas_background(&painter, rect);
            paint_empty_state(&painter, rect, false);
            paint_empty_state(&painter, rect, true);
            draw_canvas_overlays(
                &painter,
                rect,
                OverlayInfo {
                    summary: &study,
                    zoom: 0.25,
                    frame_rate: Some(FrameRateInfo {
                        fps: 45.0,
                        display_fps: 60.0,
                    }),
                    hover_base: Some(vec2(10.0, 20.0)),
                    tile_failure: Some(&failure),
                    debug_stats: Some("ready=3 pending=1"),
                },
            );
        });
        assert!(output.shapes.len() > 10);
    }

    #[test]
    fn overlay_formatting_handles_scale_and_diagnostic_boundaries() {
        assert_eq!(truncate_text("short", 10), "short");
        assert_eq!(truncate_text("abcdef", 3), "abc...");
        assert_eq!(nice_round(f32::NAN), 1.0);
        assert_eq!(nice_round(14.0), 10.0);
        assert_eq!(nice_round(24.0), 20.0);
        assert_eq!(nice_round(54.0), 50.0);
        assert_eq!(nice_round(84.0), 100.0);
        assert_eq!(fmt_zoom(1.0), "100%");
        assert_eq!(fmt_zoom(0.05), "5.0%");
        assert_eq!(fmt_zoom(0.005), "0.50%");
        assert_eq!(
            fps_color(FrameRateInfo {
                fps: 60.0,
                display_fps: 60.0,
            }),
            theme::GREEN
        );
        assert_eq!(
            fps_color(FrameRateInfo {
                fps: 50.0,
                display_fps: 60.0,
            }),
            theme::WARN
        );
        assert_eq!(
            fps_color(FrameRateInfo {
                fps: 20.0,
                display_fps: 60.0,
            }),
            theme::TEXT_MUTED
        );
    }
}
