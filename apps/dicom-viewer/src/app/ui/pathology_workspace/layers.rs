mod external;

use super::PathologyWorkspaceActions;
use crate::app::ui::section_heading;
use crate::app::{
    theme,
    workspace::{EditingRepresentation, WorkspaceRuntime},
};
use eframe::egui::{self, RichText};
use uuid::Uuid;

pub(super) fn show_layers(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
) {
    section_heading(ui, "Layers");
    let vectors = runtime
        .document()
        .vector_layers()
        .iter()
        .map(|layer| (layer.id(), layer.name().to_owned(), layer.findings().len()))
        .collect::<Vec<_>>();
    let segments = runtime
        .document()
        .segmentation_layers()
        .iter()
        .map(|layer| (layer.id(), layer.name().to_owned(), layer.segments().len()))
        .collect::<Vec<_>>();
    for (id, name, count) in vectors {
        layer_row(
            ui,
            runtime,
            actions,
            id,
            &name,
            count,
            EditingRepresentation::Vector,
        );
    }
    for (id, name, count) in segments {
        layer_row(
            ui,
            runtime,
            actions,
            id,
            &name,
            count,
            EditingRepresentation::Segmentation,
        );
    }
    external::show_external_layers(ui, runtime, actions);
}

fn edit_layer_opacity(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
    id: Uuid,
    presentation: dicom_viewer_core::LayerPresentation,
) {
    let mut opacity = presentation.opacity;
    if ui
        .add(egui::Slider::new(&mut opacity, 0.05..=1.0).text("opacity"))
        .changed()
    {
        let mut updated = presentation;
        updated.opacity = opacity;
        if let Err(error) = runtime.set_layer_presentation_without_history(id, updated) {
            actions.error = Some(error.to_string());
        }
    }
}

fn layer_row(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
    id: Uuid,
    name: &str,
    count: usize,
    representation: EditingRepresentation,
) {
    let presentation = runtime.document().presentation().layer(id);
    ui.horizontal(|ui| {
        let eye = if presentation.visible { "●" } else { "○" };
        if ui.small_button(eye).clicked() {
            if let Err(error) = runtime.set_layer_visibility(id, !presentation.visible) {
                actions.error = Some(error.to_string());
            }
        }
        let active = runtime.editing_representation() == representation;
        if ui
            .selectable_label(active, format!("{name}  {count}"))
            .clicked()
        {
            let result = match representation {
                EditingRepresentation::Vector => runtime.use_vector_layer(id),
                EditingRepresentation::Segmentation => runtime.use_segmentation_layer(id),
            };
            if let Err(error) = result {
                actions.error = Some(error.to_string());
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(if presentation.locked {
                    "locked"
                } else {
                    "editable"
                })
                .small()
                .color(theme::TEXT_DIM),
            );
        });
    });
    ui.horizontal(|ui| {
        edit_layer_opacity(ui, runtime, actions, id, presentation);
        let mut locked = presentation.locked;
        if ui.checkbox(&mut locked, "Lock").changed() {
            let mut updated = presentation;
            updated.locked = locked;
            if let Err(error) = runtime.set_layer_presentation_without_history(id, updated) {
                actions.error = Some(error.to_string());
            }
        }
    });
}
