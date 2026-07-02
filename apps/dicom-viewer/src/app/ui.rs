use dicom_viewer_core::{SourceKind, StudySummary};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Frame, Label, Margin, Rect, Response,
    RichText, Sense, Stroke, StrokeKind, TextStyle, Vec2,
};

use super::tile::TileFailureInfo;
use super::viewport::base_contains_point;
use super::{theme, FrameRateInfo, MIN_ZOOM};

pub(super) fn chrome_frame(fill: Color32, margin: Margin) -> Frame {
    Frame::NONE.fill(fill).inner_margin(margin)
}

pub(super) fn wordmark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
    let painter = ui.painter();
    let tiers = [
        (1.0_f32, theme::TEXT_DIM),
        (0.62, theme::AMBER),
        (0.30, theme::CYAN),
    ];
    let cx = rect.center().x;
    let tier_h = rect.height() / 3.0;
    for (i, (frac, color)) in tiers.iter().enumerate() {
        let y = rect.top() + i as f32 * tier_h;
        let half = rect.width() * 0.5 * frac;
        painter.rect_filled(
            Rect::from_min_max(pos2(cx - half, y + 1.0), pos2(cx + half, y + tier_h - 1.0)),
            CornerRadius::same(1),
            *color,
        );
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new("DICOM")
            .color(theme::TEXT)
            .size(14.0)
            .strong(),
    );
}

pub(super) fn rule(ui: &mut egui::Ui) {
    ui.add_space(9.0);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 18.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0, theme::HAIRLINE),
    );
    ui.add_space(9.0);
}

pub(super) fn tool_button(ui: &mut egui::Ui, label: &str) -> Response {
    ui.button(RichText::new(label).size(13.0))
}

pub(super) fn tool_toggle(ui: &mut egui::Ui, value: &mut bool, label: &str) -> Response {
    ui.toggle_value(value, RichText::new(label).size(13.0))
}

pub(super) fn privacy_badge(ui: &mut egui::Ui) {
    Frame::NONE
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::HAIRLINE))
        .inner_margin(Margin::symmetric(9, 4))
        .corner_radius(CornerRadius::same(11))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new("\u{25CF}").color(theme::GREEN).size(9.0));
            ui.label(
                RichText::new("LOCAL ONLY")
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            );
        });
}

pub(super) fn status_glyph(ui: &mut egui::Ui, opening: bool, has_study: bool) {
    let (glyph, color) = if opening {
        ("\u{25CC}", theme::CYAN)
    } else if has_study {
        ("\u{25CF}", theme::AMBER)
    } else {
        ("\u{25CB}", theme::TEXT_DIM)
    };
    ui.label(RichText::new(glyph).color(color).size(11.0));
}

fn section_header(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .color(theme::TEXT_DIM)
            .size(10.5)
            .strong(),
    );
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 7.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.top() + 3.0,
        Stroke::new(1.0, theme::HAIRLINE_SOFT),
    );
}

fn kv_row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(key).color(theme::TEXT_DIM).size(11.5));
        ui.add(
            Label::new(
                RichText::new(value)
                    .color(theme::TEXT)
                    .monospace()
                    .size(11.5),
            )
            .wrap(),
        );
    });
}

fn optional_kv_row<T: std::fmt::Display>(ui: &mut egui::Ui, key: &str, value: Option<T>) {
    if let Some(value) = value {
        kv_row(ui, key, &value.to_string());
    }
}

fn warning_row(ui: &mut egui::Ui, text: &str) {
    Frame::NONE
        .fill(Color32::from_rgb(36, 28, 15))
        .inner_margin(Margin::same(8))
        .corner_radius(CornerRadius::same(5))
        .outer_margin(Margin {
            left: 0,
            right: 0,
            top: 0,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                ui.label(RichText::new("\u{26A0}").color(theme::WARN).size(12.0));
                ui.add(
                    Label::new(
                        RichText::new(text)
                            .color(Color32::from_rgb(214, 178, 120))
                            .size(11.5),
                    )
                    .wrap(),
                );
            });
        });
}

pub(super) fn empty_facts(ui: &mut egui::Ui) {
    Frame::NONE
        .inner_margin(Margin::symmetric(16, 16))
        .show(ui, |ui| {
            section_header(ui, "No slide loaded");
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Open a whole-slide image file, or a folder of DICOM instances, to inspect its resolution pyramid.",
                )
                .color(theme::TEXT_MUTED)
                .size(12.5),
            );
        });
}

pub(super) fn facts_panel(ui: &mut egui::Ui, summary: &StudySummary) {
    Frame::NONE
        .fill(theme::CHROME_RAISED)
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.label(
                RichText::new(&summary.format_label)
                    .color(theme::TEXT)
                    .size(14.5)
                    .strong(),
            );
            let name = summary
                .source_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(summary.format_label.as_str());
            ui.add(
                Label::new(
                    RichText::new(name)
                        .color(theme::TEXT_MUTED)
                        .monospace()
                        .size(11.5),
                )
                .wrap(),
            );
        });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(14, 6))
                .show(ui, |ui| {
                    section_header(ui, "Source");
                    kv_row(
                        ui,
                        "Kind",
                        match summary.source_kind {
                            SourceKind::File => "file",
                            SourceKind::Folder => "folder",
                        },
                    );
                    kv_row(ui, "Files", &fmt_int(summary.file_count as u64));
                    if summary.dicom_instance_count > 0 {
                        kv_row(ui, "Instances", &summary.dicom_instance_count.to_string());
                    }
                    kv_row(ui, "Tile decode", &summary.tile_decode_backend.to_string());

                    if summary.mpp.is_some() || summary.objective_power.is_some() {
                        section_header(ui, "Optics");
                        if let Some((x, y)) = summary.mpp {
                            kv_row(ui, "MPP", &format!("{x:.4} \u{00D7} {y:.4} \u{00B5}m"));
                        }
                        if let Some(power) = summary.objective_power {
                            kv_row(ui, "Objective", &format!("{power:.1}\u{00D7}"));
                        }
                    }

                    if !summary.instances.is_empty() {
                        section_header(ui, "Transfer syntaxes");
                        ui.add_space(4.0);
                        let mut syntaxes = summary
                            .instances
                            .iter()
                            .map(|instance| instance.transfer_syntax_uid.as_str())
                            .collect::<Vec<_>>();
                        syntaxes.sort_unstable();
                        syntaxes.dedup();
                        for uid in syntaxes {
                            ui.add(
                                Label::new(
                                    RichText::new(uid)
                                        .color(theme::TEXT_MUTED)
                                        .monospace()
                                        .size(11.0),
                                )
                                .wrap(),
                            );
                        }

                        section_header(
                            ui,
                            &format!("Instances \u{00B7} {}", summary.instances.len()),
                        );
                        for instance in &summary.instances {
                            let name = instance
                                .path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("<unnamed>");
                            egui::CollapsingHeader::new(RichText::new(name).monospace().size(11.5))
                                .default_open(false)
                                .show(ui, |ui| {
                                    kv_row(ui, "SOP class", &instance.sop_class_uid);
                                    kv_row(ui, "Transfer", &instance.transfer_syntax_uid);
                                    if !instance.image_type.is_empty() {
                                        kv_row(ui, "Image type", &instance.image_type.join(" / "));
                                    }
                                    if let (Some(cols), Some(rows)) = (
                                        instance.total_pixel_matrix_columns,
                                        instance.total_pixel_matrix_rows,
                                    ) {
                                        kv_row(ui, "Matrix", &format!("{cols} \u{00D7} {rows}"));
                                    }
                                    if let (Some(cols), Some(rows)) =
                                        (instance.columns, instance.rows)
                                    {
                                        kv_row(ui, "Frame", &format!("{cols} \u{00D7} {rows}"));
                                    }
                                    if let Some(frames) = instance.number_of_frames {
                                        kv_row(ui, "Frames", &frames.to_string());
                                    }
                                    optional_kv_row(
                                        ui,
                                        "Organization",
                                        instance.dimension_organization_type.as_deref(),
                                    );
                                    if let Some((x, y)) = instance.pixel_spacing {
                                        kv_row(
                                            ui,
                                            "Spacing",
                                            &format!("{x:.6} \u{00D7} {y:.6} mm"),
                                        );
                                    }
                                    optional_kv_row(
                                        ui,
                                        "Photometric",
                                        instance.photometric_interpretation.as_deref(),
                                    );
                                    optional_kv_row(ui, "Samples", instance.samples_per_pixel);
                                    optional_kv_row(ui, "Bits stored", instance.bits_stored);
                                    optional_kv_row(ui, "Bits allocated", instance.bits_allocated);
                                    optional_kv_row(ui, "High bit", instance.high_bit);
                                    optional_kv_row(
                                        ui,
                                        "Pixel repr",
                                        instance.pixel_representation,
                                    );
                                    optional_kv_row(
                                        ui,
                                        "Planar config",
                                        instance.planar_configuration,
                                    );
                                });
                        }
                    }

                    if !summary.warnings.is_empty() {
                        section_header(ui, "Warnings");
                        ui.add_space(4.0);
                        for warning in &summary.warnings {
                            warning_row(ui, warning);
                        }
                    }

                    ui.add_space(14.0);
                });
        });
}

pub(super) fn paint_canvas_background(painter: &egui::Painter, rect: Rect) {
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

pub(super) fn paint_empty_state(painter: &egui::Painter, rect: Rect, opening: bool) {
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

pub(super) fn draw_canvas_overlays(
    painter: &egui::Painter,
    rect: Rect,
    summary: &StudySummary,
    zoom: f32,
    frame_rate: Option<FrameRateInfo>,
    hover_base: Option<Vec2>,
    tile_failure: Option<&TileFailureInfo>,
) {
    let font = FontId::monospace(12.0);
    let mut parts: Vec<(String, Color32)> = Vec::new();
    parts.push((fmt_zoom(zoom), theme::CYAN));
    if let Some(frame_rate) = frame_rate {
        parts.push((format!("{:.0} fps", frame_rate.fps), fps_color(frame_rate)));
    }
    if let Some(base) = hover_base {
        if base_contains_point(summary, base) {
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

    draw_scale_bar(painter, rect, summary, zoom);
    if let Some(failure) = tile_failure {
        draw_tile_failure_badge(painter, rect, failure);
    }
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

pub(super) fn fps_color(frame_rate: FrameRateInfo) -> Color32 {
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
        (format!("{} px", fmt_int(nice as u64)), bar_px)
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

fn fmt_int(value: u64) -> String {
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

pub(super) fn install_visuals(ctx: &egui::Context) {
    let mut style = (*ctx.global_style()).clone();

    style.text_styles = [
        (TextStyle::Heading, FontId::proportional(17.0)),
        (TextStyle::Body, FontId::proportional(13.5)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Monospace, FontId::monospace(12.5)),
    ]
    .into();

    style.spacing.item_spacing = vec2(8.0, 7.0);
    style.spacing.button_padding = vec2(10.0, 5.0);
    style.spacing.window_margin = Margin::same(0);
    style.spacing.menu_margin = Margin::same(6);
    style.spacing.interact_size.y = 26.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = theme::CHROME;
    v.window_fill = theme::CARD;
    v.window_stroke = Stroke::new(1.0, theme::HAIRLINE);
    v.window_corner_radius = CornerRadius::same(8);
    v.faint_bg_color = theme::CARD_RAISED;
    v.extreme_bg_color = theme::CANVAS_EDGE;
    v.override_text_color = Some(theme::TEXT);
    v.hyperlink_color = theme::CYAN;

    v.selection.bg_fill = theme::AMBER_GLOW;
    v.selection.stroke = Stroke::new(1.0, theme::AMBER);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = theme::CHROME;
    w.noninteractive.weak_bg_fill = theme::CHROME;
    w.noninteractive.bg_stroke = Stroke::new(1.0, theme::HAIRLINE_SOFT);
    w.noninteractive.fg_stroke = Stroke::new(1.0, theme::TEXT_MUTED);
    w.noninteractive.corner_radius = CornerRadius::same(6);

    w.inactive.bg_fill = theme::CARD_RAISED;
    w.inactive.weak_bg_fill = theme::CHROME_RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, theme::HAIRLINE);
    w.inactive.fg_stroke = Stroke::new(1.0, theme::TEXT);
    w.inactive.corner_radius = CornerRadius::same(6);

    w.hovered.bg_fill = theme::AMBER_GLOW;
    w.hovered.weak_bg_fill = theme::AMBER_GLOW;
    w.hovered.bg_stroke = Stroke::new(1.0, theme::AMBER);
    w.hovered.fg_stroke = Stroke::new(1.0, theme::AMBER_BRIGHT);
    w.hovered.corner_radius = CornerRadius::same(6);

    w.active.bg_fill = theme::AMBER;
    w.active.weak_bg_fill = theme::AMBER;
    w.active.bg_stroke = Stroke::new(1.0, theme::AMBER_BRIGHT);
    w.active.fg_stroke = Stroke::new(1.0, theme::CANVAS_EDGE);
    w.active.corner_radius = CornerRadius::same(6);

    w.open.bg_fill = theme::CARD_RAISED;
    w.open.bg_stroke = Stroke::new(1.0, theme::HAIRLINE);
    w.open.fg_stroke = Stroke::new(1.0, theme::TEXT);

    ctx.set_global_style(style);
}
