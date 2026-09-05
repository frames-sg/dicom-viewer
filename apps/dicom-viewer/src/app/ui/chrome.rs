use eframe::egui::{
    self, pos2, vec2, Align, Color32, CornerRadius, Frame, Layout, Margin, Panel, Rect, RichText,
    Sense, Stroke,
};

use dicom_viewer_core::StudySummary;

use super::super::theme;
use super::super::WheelZoomSettings;

#[derive(Debug, Default)]
pub(in crate::app) struct ToolbarActions {
    pub(in crate::app) open_file: bool,
    pub(in crate::app) open_folder: bool,
    pub(in crate::app) fit: bool,
    pub(in crate::app) zoom_out: bool,
    pub(in crate::app) zoom_in: bool,
    pub(in crate::app) import: bool,
    pub(in crate::app) export: bool,
    pub(in crate::app) undo: bool,
    pub(in crate::app) redo: bool,
    pub(in crate::app) cancel_export: bool,
}

pub(in crate::app) struct ToolbarState<'a> {
    pub(in crate::app) has_study: bool,
    pub(in crate::app) show_facts: &'a mut bool,
    pub(in crate::app) show_pathology: &'a mut bool,
    pub(in crate::app) can_undo: bool,
    pub(in crate::app) can_redo: bool,
    pub(in crate::app) autosave_status: &'a str,
    pub(in crate::app) export_running: bool,
    pub(in crate::app) export_cancel_requested: bool,
    pub(in crate::app) smooth_camera: &'a mut bool,
    pub(in crate::app) wheel_zoom: &'a mut WheelZoomSettings,
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
                ui.menu_button(RichText::new("Open").size(13.0), |ui| {
                    if ui.button("Open file…").clicked() {
                        actions.open_file = true;
                        ui.close();
                    }
                    if ui.button("Open DICOM folder…").clicked() {
                        actions.open_folder = true;
                        ui.close();
                    }
                });
                rule(ui);
                ui.toggle_value(state.show_facts, RichText::new("Info").size(13.0));
                if state.has_study {
                    rule(ui);
                    ui.toggle_value(
                        state.show_pathology,
                        RichText::new("Annotations").size(13.0),
                    );
                    actions.import = ui.button(RichText::new("Import").size(13.0)).clicked();
                    if state.export_running {
                        actions.cancel_export = ui
                            .add_enabled(
                                !state.export_cancel_requested,
                                egui::Button::new(if state.export_cancel_requested {
                                    "Cancelling…"
                                } else {
                                    "Cancel Export"
                                }),
                            )
                            .clicked();
                    } else {
                        actions.export = ui.button(RichText::new("Export").size(13.0)).clicked();
                    }
                    rule(ui);
                    actions.undo = ui
                        .add_enabled(state.can_undo, egui::Button::new("Undo"))
                        .clicked();
                    actions.redo = ui
                        .add_enabled(state.can_redo, egui::Button::new("Redo"))
                        .clicked();
                    rule(ui);
                    actions.fit = ui.button(RichText::new("Fit").size(13.0)).clicked();
                    actions.zoom_out = ui.button(RichText::new("\u{2212}").size(13.0)).clicked();
                    actions.zoom_in = ui.button(RichText::new("+").size(13.0)).clicked();
                    ui.menu_button("View", |ui| {
                        ui.checkbox(state.smooth_camera, "Smooth navigation");
                        ui.checkbox(
                            state.wheel_zoom.inverted_mut(),
                            "Invert wheel zoom direction",
                        );
                        ui.add(
                            egui::Slider::new(state.wheel_zoom.speed_mut(), 0.25..=4.0)
                                .logarithmic(true)
                                .custom_formatter(|value, _| format!("{value:.2}×"))
                                .text("Wheel zoom speed"),
                        );
                    });
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    privacy_badge(ui);
                    if state.has_study {
                        ui.label(
                            RichText::new(state.autosave_status)
                                .size(11.0)
                                .color(theme::TEXT_MUTED),
                        );
                    }
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
            ui.label(RichText::new("LOCAL").color(theme::TEXT_MUTED).size(11.0));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{run_ui, summary};

    #[test]
    fn toolbar_and_status_bar_render_each_availability_state_without_actions() {
        let mut show_facts = false;
        let mut show_pathology = false;
        let mut smooth_camera = true;
        let mut wheel_zoom = WheelZoomSettings::for_os("windows");
        let output = run_ui(|ui| {
            let actions = show_toolbar(
                ui,
                ToolbarState {
                    has_study: false,
                    show_facts: &mut show_facts,
                    show_pathology: &mut show_pathology,
                    can_undo: false,
                    can_redo: false,
                    autosave_status: "Not saved",
                    export_running: false,
                    export_cancel_requested: false,
                    smooth_camera: &mut smooth_camera,
                    wheel_zoom: &mut wheel_zoom,
                },
            );
            assert!(!actions.open_file);
            assert!(!actions.open_folder);
            show_status_bar(ui, "Open a slide", false, None);
        });
        assert!(!output.shapes.is_empty());

        let study = summary();
        let output = run_ui(|ui| {
            let actions = show_toolbar(
                ui,
                ToolbarState {
                    has_study: true,
                    show_facts: &mut show_facts,
                    show_pathology: &mut show_pathology,
                    can_undo: true,
                    can_redo: true,
                    autosave_status: "Saved",
                    export_running: false,
                    export_cancel_requested: false,
                    smooth_camera: &mut smooth_camera,
                    wheel_zoom: &mut wheel_zoom,
                },
            );
            assert!(!actions.undo);
            assert!(!actions.redo);
            show_status_bar(ui, "Opening", true, Some(&study));
        });
        assert!(!output.shapes.is_empty());
    }
}
