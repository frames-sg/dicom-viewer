use super::{edit_layer_opacity, PathologyWorkspaceActions};
use crate::app::{
    theme,
    workspace::{ExternalClassDescriptor, WorkspaceRuntime},
};
use dicom_viewer_core::{AnnotationClassGeometry, ExternalLayerReference};
use eframe::egui::{self, RichText, ScrollArea};

pub(super) fn show_external_layers(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
) {
    let layers = runtime.document().external_layers().to_vec();
    for layer in &layers {
        show_external_layer(ui, runtime, actions, layer);
    }
}

fn show_external_layer(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
    layer: &ExternalLayerReference,
) {
    let id = layer.id();
    let name = layer.name();
    let kind = layer.kind();
    let source_path = layer.source_path();
    let count = layer.source_object_count();
    let presentation = runtime.document().presentation().layer(id);
    let mut remove_layer = false;
    let mut load_layer = false;
    let payload_loaded = runtime.external_payload(id).is_some();
    ui.horizontal(|ui| {
        let eye = if presentation.visible { "●" } else { "○" };
        if ui.small_button(eye).clicked() {
            if let Err(error) = runtime.set_layer_visibility(id, !presentation.visible) {
                actions.error = Some(error.to_string());
            }
        }
        ui.label(RichText::new("◇").color(theme::TEXT_DIM));
        ui.label(format!("{name} · {count}"));
        ui.label(RichText::new("LOCKED").small().color(theme::TEXT_DIM));
        if !payload_loaded
            && source_path.is_some()
            && sidecar_kind_for_external(kind).is_some()
            && ui.small_button("Load").clicked()
        {
            load_layer = true;
        }
        if ui.small_button("Remove").clicked() {
            remove_layer = true;
        }
    });
    ui.horizontal(|ui| {
        edit_layer_opacity(ui, runtime, actions, id, presentation);
        let status = source_path.map_or("embedded reference", |path| {
            if path.exists() {
                "linked source"
            } else {
                "missing linked source"
            }
        });
        ui.label(RichText::new(format!("{kind:?} · {status}")).small().color(
            if status.starts_with("missing") {
                theme::AMBER
            } else {
                theme::TEXT_DIM
            },
        ));
    });
    if remove_layer {
        match runtime.remove_external_layer(id) {
            Ok(true) => {
                actions.status = Some(format!("Removed source layer {name}; Undo restores it."));
            }
            Ok(false) => {}
            Err(error) => actions.error = Some(error.to_string()),
        }
        return;
    }
    if load_layer {
        actions.load_sidecar = source_path
            .map(std::path::Path::to_path_buf)
            .zip(sidecar_kind_for_external(kind));
    }
    let classes = runtime.external_classes(id);
    ui.indent(("external-controls", id), |ui| match classes {
        Err(error) => {
            ui.label(RichText::new(error.to_string()).small().color(theme::AMBER));
        }
        Ok(classes) if classes.is_empty() => {
            let message =
                if *kind == dicom_viewer_core::ExternalLayerKind::Heatmap && payload_loaded {
                    "Heatmap source result; export it through DICOM PM."
                } else {
                    "Source payload is unloaded or has no lossless editable objects."
                };
            ui.label(RichText::new(message).small().color(theme::TEXT_DIM));
        }
        Ok(classes) => {
            ui.collapsing("Class mapping & promotion", |ui| {
                show_promotion(ui, runtime, actions, layer, &classes);
            });
        }
    });
}

fn sidecar_kind_for_external(
    kind: &dicom_viewer_core::ExternalLayerKind,
) -> Option<dicom_viewer_core::SidecarKind> {
    match kind {
        dicom_viewer_core::ExternalLayerKind::DicomAnn => {
            Some(dicom_viewer_core::SidecarKind::Annotation)
        }
        dicom_viewer_core::ExternalLayerKind::DicomSeg => {
            Some(dicom_viewer_core::SidecarKind::BinarySegmentation)
        }
        dicom_viewer_core::ExternalLayerKind::DicomSr => {
            Some(dicom_viewer_core::SidecarKind::StructuredReport)
        }
        dicom_viewer_core::ExternalLayerKind::ProfiledGeoJson
        | dicom_viewer_core::ExternalLayerKind::Heatmap => None,
    }
}

fn show_promotion(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
    layer: &ExternalLayerReference,
    classes: &[ExternalClassDescriptor],
) {
    let id = layer.id();
    let mappings = layer.class_mappings();
    let scheme_options = runtime
        .document()
        .scheme()
        .classes()
        .iter()
        .map(|class| {
            (
                class.id().to_owned(),
                class.label().to_owned(),
                class.geometry(),
            )
        })
        .collect::<Vec<_>>();
    for class in classes {
        show_class_mapping(ui, runtime, actions, layer, class, &scheme_options);
    }

    let complete = classes
        .iter()
        .all(|class| class.editable && mappings.contains_key(&class.key));
    if ui
        .add_enabled(complete, egui::Button::new("Make editable"))
        .on_hover_text("Convert every source object only after every source class is mapped")
        .clicked()
    {
        match runtime.make_external_layer_editable(id) {
            Ok(ids) => {
                actions.status = Some(format!(
                    "Converted {} source object(s) as independently tracked findings.",
                    ids.len()
                ));
            }
            Err(error) => actions.error = Some(error.to_string()),
        }
    }

    ui.collapsing("Source objects", |ui| {
        show_source_objects(ui, runtime, actions, layer)
    });
}

fn show_class_mapping(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
    layer: &ExternalLayerReference,
    class: &ExternalClassDescriptor,
    scheme_options: &[(String, String, AnnotationClassGeometry)],
) {
    let id = layer.id();
    let mappings = layer.class_mappings();

    ui.horizontal_wrapped(|ui| {
        ui.label(format!(
            "{} · {} {}",
            class.label,
            class.object_count,
            class.geometry.label().to_lowercase()
        ));
        if !class.editable {
            ui.label(
                RichText::new("read-only geometry")
                    .small()
                    .color(theme::AMBER),
            );
        }
    });
    let current = mappings.get(&class.key).cloned();
    let selected_text = current
        .as_deref()
        .and_then(|id| {
            scheme_options
                .iter()
                .find(|(candidate, _, _)| candidate == id)
                .map(|(_, label, _)| label.as_str())
        })
        .unwrap_or("Unmapped");
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("external-class-map", id, &class.key))
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                for (target_id, label, geometry) in scheme_options {
                    if *geometry != class.geometry {
                        continue;
                    }
                    if ui
                        .selectable_label(current.as_deref() == Some(target_id.as_str()), label)
                        .clicked()
                    {
                        if let Err(error) =
                            runtime.set_external_class_mapping(id, &class.key, target_id)
                        {
                            actions.error = Some(error.to_string());
                        }
                    }
                }
            });
        if current.is_none() {
            if let Some(suggested) = class.exact_scheme_class_id.as_deref() {
                let label = scheme_options
                    .iter()
                    .find(|(id, _, _)| id == suggested)
                    .map_or(suggested, |(_, label, _)| label.as_str());
                if ui.small_button(format!("Use exact: {label}")).clicked() {
                    if let Err(error) =
                        runtime.set_external_class_mapping(id, &class.key, suggested)
                    {
                        actions.error = Some(error.to_string());
                    }
                }
            }
        }
    });
}

fn show_source_objects(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
    layer: &ExternalLayerReference,
) {
    let id = layer.id();
    let mappings = layer.class_mappings();
    match runtime.external_objects(id) {
        Err(error) => {
            ui.label(RichText::new(error.to_string()).small().color(theme::AMBER));
        }
        Ok(objects) => {
            ScrollArea::vertical()
                .id_salt(("external-objects", id))
                .max_height(180.0)
                .show_rows(ui, 24.0, objects.len(), |ui, range| {
                    for object in &objects[range] {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&object.label)
                                    .small()
                                    .color(theme::TEXT_MUTED),
                            );
                            let mapped = mappings.contains_key(&object.class_key);
                            let enabled = object.promotable && mapped && !object.promoted;
                            let response = ui.add_enabled(
                                enabled,
                                egui::Button::new(if object.promoted {
                                    "Tracked"
                                } else {
                                    "Promote"
                                }),
                            );
                            let response =
                                if let Some(reason) = object.promotion_block_reason.as_deref() {
                                    response.on_disabled_hover_text(reason)
                                } else {
                                    response
                                };
                            if response.clicked() {
                                match runtime.promote_external_object(id, &object.source_object_id)
                                {
                                    Ok(_) => {
                                        actions.status = Some(
                                            "Promoted one source object as a tracked finding."
                                                .into(),
                                        );
                                    }
                                    Err(error) => {
                                        actions.error = Some(error.to_string());
                                    }
                                }
                            }
                        });
                    }
                });
        }
    }
}
