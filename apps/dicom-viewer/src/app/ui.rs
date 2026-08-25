pub(super) mod chrome;
pub(super) mod facts;
pub(super) mod overlay;
pub(super) mod pathology_workspace;

use eframe::egui::{self, RichText};

use super::theme;

pub(super) fn section_heading(ui: &mut egui::Ui, label: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(label.to_uppercase())
            .color(theme::TEXT_DIM)
            .small()
            .strong(),
    );
}
