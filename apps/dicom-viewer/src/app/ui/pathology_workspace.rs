mod findings;
mod inspector;
mod layers;
mod palette;
mod tool_rail;

use std::path::PathBuf;

use dicom_viewer_core::{AnnotationClassGeometry, SegmentOperation, WorkspaceObjectGeometryKind};
use eframe::egui::{self, Color32, Margin, Panel, RichText, ScrollArea, Stroke};
use uuid::Uuid;

use super::super::theme;
use super::super::workspace::{
    ActiveTool, DraftResolution, EditingRepresentation, ToolTransitionOutcome, WorkspaceRuntime,
};
use super::chrome::chrome_frame;
use super::section_heading;

use findings::{finding_rows, show_findings};
use inspector::show_inspector;
use layers::show_layers;
use palette::show_scheme_and_palette;
#[cfg(test)]
use palette::visible_palette;
pub(in crate::app) use tool_rail::show_tool_rail;

const TOOL_RAIL_WIDTH: f32 = 58.0;
const FINDING_ROW_HEIGHT: f32 = 25.0;

#[derive(Debug, Default)]
pub(in crate::app) struct PathologyWorkspaceActions {
    pub(in crate::app) close_panel: bool,
    pub(in crate::app) export_current_view_tiff: bool,
    pub(in crate::app) import_dicom: bool,
    pub(in crate::app) import_profiled_geojson: bool,
    pub(in crate::app) import_sr: bool,
    pub(in crate::app) import_sr_with_seg: bool,
    pub(in crate::app) import_raster: bool,
    pub(in crate::app) load_sidecar: Option<(PathBuf, dicom_viewer_core::SidecarKind)>,
    pub(in crate::app) export_ann: bool,
    pub(in crate::app) export_compatibility_ann: bool,
    pub(in crate::app) export_seg: bool,
    pub(in crate::app) export_sr: bool,
    pub(in crate::app) export_pm: bool,
    pub(in crate::app) export_scheme_geojson: bool,
    pub(in crate::app) export_compatibility_geojson: bool,
    pub(in crate::app) export_portable_workspace: bool,
    pub(in crate::app) install_scheme: bool,
    pub(in crate::app) workspace_storage: bool,
    pub(in crate::app) delete_selection: bool,
    pub(in crate::app) jump_to: Option<Uuid>,
    pub(in crate::app) error: Option<String>,
    pub(in crate::app) status: Option<String>,
}

pub(in crate::app) fn show_populated_pathology_workspace_panel(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
) -> Option<PathologyWorkspaceActions> {
    (runtime.document().object_count() > 0).then(|| show_pathology_workspace_panel(ui, runtime))
}

pub(in crate::app) fn show_pathology_workspace_panel(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
) -> PathologyWorkspaceActions {
    let mut actions = PathologyWorkspaceActions::default();
    Panel::right("pathology-workspace")
        .resizable(true)
        .default_size(342.0)
        .size_range(304.0..=520.0)
        .frame(chrome_frame(theme::CHROME, Margin::same(10)))
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Pathology").color(theme::TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    actions.close_panel = ui
                        .small_button("X")
                        .on_hover_text("Close annotations")
                        .clicked();
                    let count = runtime.document().object_count();
                    ui.label(
                        RichText::new(format!("{count} tracked"))
                            .small()
                            .color(theme::TEXT_DIM),
                    );
                });
            });
            ui.label(
                RichText::new("Tracked findings, editable segments, and source results.")
                    .small()
                    .color(theme::TEXT_MUTED),
            );
            if runtime.history_truncated() {
                ui.label(
                    RichText::new(
                        "Older undo history was truncated to stay within resource limits.",
                    )
                    .small()
                    .color(theme::AMBER),
                );
            } else if let Some(label) = runtime.undo_label() {
                ui.label(
                    RichText::new(format!("Undo: {label}"))
                        .small()
                        .color(theme::TEXT_DIM),
                );
            }
            if let Some(label) = runtime.redo_label() {
                ui.label(
                    RichText::new(format!("Redo: {label}"))
                        .small()
                        .color(theme::TEXT_DIM),
                );
            }

            show_draft_guard(ui, runtime, &mut actions);
            show_scheme_and_palette(ui, runtime, &mut actions);
            show_findings(ui, runtime, &mut actions);
            show_layers(ui, runtime, &mut actions);
            show_inspector(ui, runtime, &mut actions);
            show_expert(ui, runtime, &mut actions);
        });
    actions
}

fn show_draft_guard(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
) {
    if runtime.draft().is_none() {
        return;
    }
    egui::Frame::NONE
        .fill(theme::AMBER_GLOW)
        .stroke(Stroke::new(1.0, theme::AMBER))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.label(RichText::new("Unfinished polygon").strong());
            ui.horizontal(|ui| {
                for (label, resolution) in [
                    ("Resume", DraftResolution::Resume),
                    ("Finish", DraftResolution::Finish),
                    ("Discard", DraftResolution::Discard),
                ] {
                    if ui.button(label).clicked() {
                        if let Err(error) = runtime.resolve_draft_transition(resolution) {
                            actions.error = Some(error.to_string());
                        }
                    }
                }
            });
        });
}

fn show_expert(
    ui: &mut egui::Ui,
    runtime: &WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
) {
    ui.add_space(8.0);
    ui.collapsing("Terminology & DICOM", |ui| {
        ui.label(
            RichText::new("Controlled concepts are read-only in this workspace.")
                .small()
                .color(theme::TEXT_MUTED),
        );
        if let Some(class) = runtime.document().scheme().class(runtime.active_class_id()) {
            ui.monospace(format!(
                "Category  {}:{}  {}",
                class.category().scheme(),
                class.category().value(),
                class.category().meaning()
            ));
            ui.monospace(format!(
                "Property  {}:{}  {}",
                class.property_type().scheme(),
                class.property_type().value(),
                class.property_type().meaning()
            ));
        }
        actions.install_scheme = ui.button("Annotation Schemes…").clicked();
        actions.workspace_storage = ui.button("Workspace Storage…").clicked();
    });
}

#[cfg(test)]
mod tests;
