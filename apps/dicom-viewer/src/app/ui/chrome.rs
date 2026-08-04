use eframe::egui::{
    self, pos2, vec2, Align, Color32, CornerRadius, Frame, Layout, Margin, Panel, Rect, RichText,
    Sense, Stroke,
};

use dicom_viewer_core::StudySummary;

use super::super::annotation::AnnotationMode;
use super::super::theme;

#[derive(Debug, Default)]
pub(in crate::app) struct ToolbarActions {
    pub(in crate::app) open_file: bool,
    pub(in crate::app) open_folder: bool,
    pub(in crate::app) fit: bool,
    pub(in crate::app) zoom_out: bool,
    pub(in crate::app) zoom_in: bool,
    pub(in crate::app) measure_clicked: bool,
    pub(in crate::app) annotate_clicked: bool,
    pub(in crate::app) annotation_mode: Option<AnnotationMode>,
    pub(in crate::app) close_polygon: bool,
    pub(in crate::app) undo_annotation: bool,
    pub(in crate::app) export_annotations: bool,
}

pub(in crate::app) struct ToolbarState<'a> {
    pub(in crate::app) has_study: bool,
    pub(in crate::app) show_facts: &'a mut bool,
    pub(in crate::app) measurement_active: &'a mut bool,
    pub(in crate::app) annotation_active: &'a mut bool,
    pub(in crate::app) annotation_mode: AnnotationMode,
    pub(in crate::app) has_annotation_work: bool,
    pub(in crate::app) has_open_polygon: bool,
    pub(in crate::app) has_exportable_annotations: bool,
    pub(in crate::app) smooth_camera: &'a mut bool,
}

pub(in crate::app) fn show_toolbar(ui: &mut egui::Ui, state: ToolbarState<'_>) -> ToolbarActions {
    let mut actions = ToolbarActions::default();
    Panel::top("toolbar")
        .exact_size(46.0)
        .frame(chrome_frame(theme::CHROME_RAISED, Margin::symmetric(12, 0)))
        .show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                wordmark(ui);
                rule(ui);
                actions.open_file = ui.button(RichText::new("Open file").size(13.0)).clicked();
                actions.open_folder = ui.button(RichText::new("Open folder").size(13.0)).clicked();
                rule(ui);
                ui.toggle_value(state.show_facts, RichText::new("Info").size(13.0));
                if state.has_study {
                    rule(ui);
                    actions.fit = ui.button(RichText::new("Fit").size(13.0)).clicked();
                    actions.zoom_out = ui.button(RichText::new("\u{2212}").size(13.0)).clicked();
                    actions.zoom_in = ui.button(RichText::new("+").size(13.0)).clicked();
                    actions.measure_clicked = ui
                        .toggle_value(
                            state.measurement_active,
                            RichText::new("Measure").size(13.0),
                        )
                        .clicked();
                    actions.annotate_clicked = ui
                        .toggle_value(
                            state.annotation_active,
                            RichText::new("Annotate").size(13.0),
                        )
                        .clicked();
                    if *state.annotation_active {
                        actions.annotation_mode = [AnnotationMode::Tumor, AnnotationMode::Hole]
                            .into_iter()
                            .find(|mode| {
                                ui.selectable_label(
                                    state.annotation_mode == *mode,
                                    RichText::new(mode.label()).size(12.0),
                                )
                                .clicked()
                            });
                        actions.close_polygon = ui
                            .add_enabled(state.has_open_polygon, egui::Button::new("Close"))
                            .clicked();
                        actions.undo_annotation = ui
                            .add_enabled(state.has_annotation_work, egui::Button::new("Undo"))
                            .clicked();
                        actions.export_annotations = ui
                            .add_enabled(
                                state.has_exportable_annotations,
                                egui::Button::new("Save GeoJSON"),
                            )
                            .clicked();
                    }
                    ui.toggle_value(state.smooth_camera, RichText::new("Smooth").size(13.0));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    privacy_badge(ui);
                });
            });
        });
    actions
}

pub(in crate::app) fn show_status_bar(
    ui: &mut egui::Ui,
    status: &str,
    opening: bool,
    summary: Option<&StudySummary>,
) {
    Panel::bottom("status")
        .exact_size(28.0)
        .frame(chrome_frame(theme::CHROME, Margin::symmetric(12, 0)))
        .show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                status_glyph(ui, opening, summary.is_some());
                ui.label(RichText::new(status).color(theme::TEXT_MUTED).size(12.0));
                if let Some(summary) = summary {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} file{}",
                                summary.file_count,
                                if summary.file_count == 1 { "" } else { "s" }
                            ))
                            .color(theme::TEXT_DIM)
                            .size(12.0),
                        );
                    });
                }
            });
        });
}

pub(in crate::app) fn chrome_frame(fill: Color32, margin: Margin) -> Frame {
    Frame::NONE.fill(fill).inner_margin(margin)
}

pub(in crate::app) fn wordmark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
    let tiers = [
        (1.0_f32, theme::TEXT_DIM),
        (0.62, theme::AMBER),
        (0.30, theme::CYAN),
    ];
    let center_x = rect.center().x;
    let tier_height = rect.height() / 3.0;
    for (index, (fraction, color)) in tiers.iter().enumerate() {
        let y = rect.top() + index as f32 * tier_height;
        let half_width = rect.width() * 0.5 * fraction;
        ui.painter().rect_filled(
            Rect::from_min_max(
                pos2(center_x - half_width, y + 1.0),
                pos2(center_x + half_width, y + tier_height - 1.0),
            ),
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

pub(in crate::app) fn rule(ui: &mut egui::Ui) {
    ui.add_space(9.0);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 18.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0, theme::HAIRLINE),
    );
    ui.add_space(9.0);
}

pub(in crate::app) fn privacy_badge(ui: &mut egui::Ui) {
    Frame::NONE
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::HAIRLINE))
        .inner_margin(Margin::symmetric(9, 4))
        .corner_radius(CornerRadius::same(11))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new("\u{25CF}").color(theme::GREEN).size(9.0));
            ui.label(
                RichText::new("RESEARCH · LOCAL")
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            );
        });
}

pub(in crate::app) fn status_glyph(ui: &mut egui::Ui, opening: bool, has_study: bool) {
    let (glyph, color) = if opening {
        ("\u{25CC}", theme::CYAN)
    } else if has_study {
        ("\u{25CF}", theme::AMBER)
    } else {
        ("\u{25CB}", theme::TEXT_DIM)
    };
    ui.label(RichText::new(glyph).color(color).size(11.0));
}
