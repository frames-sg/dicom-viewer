use std::path::PathBuf;

use dicom_viewer_core::{AnnotationClassGeometry, SegmentOperation, VectorFindingGeometry};
use eframe::egui::{self, Color32, Margin, Panel, RichText, ScrollArea, Stroke};
use uuid::Uuid;

use super::super::theme;
use super::super::workspace::{
    ActiveTool, DraftResolution, EditingRepresentation, ToolTransitionOutcome, WorkspaceRuntime,
};
use super::chrome::chrome_frame;
use super::section_heading;

const TOOL_RAIL_WIDTH: f32 = 58.0;
const FINDING_ROW_HEIGHT: f32 = 25.0;

#[derive(Debug, Default)]
pub(in crate::app) struct PathologyWorkspaceActions {
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

pub(in crate::app) fn show_tool_rail(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
) -> Option<String> {
    let mut error = None;
    Panel::left("pathology-tool-rail")
        .exact_size(TOOL_RAIL_WIDTH)
        .frame(chrome_frame(theme::CHROME, Margin::symmetric(6, 8)))
        .show_inside(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("TOOLS")
                        .size(9.0)
                        .color(theme::TEXT_DIM)
                        .strong(),
                );
                ui.add_space(3.0);
                for tool in ActiveTool::ALL {
                    let selected = runtime.active_tool() == tool;
                    let response = ui
                        .selectable_label(
                            selected,
                            RichText::new(tool_glyph(tool)).size(17.0).strong(),
                        )
                        .on_hover_text(format!("{} ({})", tool.label(), tool.shortcut()));
                    if response.clicked() {
                        if tool == ActiveTool::Brush {
                            if let Err(err) = runtime.ensure_segmentation_layer() {
                                error = Some(err.to_string());
                                continue;
                            }
                        }
                        if runtime.request_tool(tool) == ToolTransitionOutcome::BlockedByDraft {
                            error = Some(
                                "Unfinished polygon: choose Resume, Finish, or Discard.".into(),
                            );
                        }
                    }
                }
                ui.add_space(5.0);
                ui.separator();
                if ui
                    .button(RichText::new("N").monospace().strong())
                    .on_hover_text("New independent finding / segment (N)")
                    .clicked()
                {
                    runtime.begin_new_segment();
                }
            });
        });
    error
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

fn show_scheme_and_palette(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
) {
    section_heading(ui, "Annotation scheme");
    let scheme = runtime.document().scheme();
    ui.horizontal(|ui| {
        ui.label(RichText::new(scheme.display_name()).strong());
        ui.label(
            RichText::new(format!("v{}", scheme.version()))
                .small()
                .color(theme::TEXT_DIM),
        );
    });
    ui.label(
        RichText::new(format!(
            "{} · {}…",
            scheme.id(),
            &scheme.content_digest()[..12]
        ))
        .monospace()
        .small()
        .color(theme::TEXT_DIM),
    );
    let palette = visible_palette(runtime);
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for (id, label, color, selected) in palette {
            let color = Color32::from_rgb(color[0], color[1], color[2]);
            let response = ui
                .horizontal(|ui| {
                    let (swatch, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 16.0), egui::Sense::hover());
                    ui.painter().rect_filled(swatch, 1.0, color);
                    ui.selectable_label(selected, RichText::new(label).size(12.0))
                })
                .inner;
            if response.clicked() {
                if let Err(error) = runtime.set_active_class(id) {
                    actions.error = Some(error.to_string());
                }
            }
        }
    });

    if runtime.editing_representation() == EditingRepresentation::Segmentation
        && matches!(
            runtime.active_tool(),
            ActiveTool::Polygon | ActiveTool::Brush
        )
    {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Operation").small().color(theme::TEXT_MUTED));
            for (operation, label) in [
                (SegmentOperation::Add, "Add"),
                (SegmentOperation::Erase, "Erase"),
            ] {
                let enabled = operation == SegmentOperation::Add
                    || runtime
                        .selection()
                        .iter()
                        .any(|id| runtime.document().segment(*id).is_some());
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::selectable(runtime.segment_operation() == operation, label),
                    )
                    .clicked()
                {
                    runtime.set_segment_operation(operation);
                }
            }
            ui.label(RichText::new("Alt reverses").small().color(theme::TEXT_DIM));
        });
        if runtime.active_tool() == ActiveTool::Brush {
            ui.label(
                RichText::new(format!("Brush Ø {:.0} px", runtime.brush_diameter()))
                    .small()
                    .color(theme::TEXT_MUTED),
            );
        }
    }
}

fn show_findings(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
) {
    section_heading(ui, "Findings");
    let rows = finding_rows(runtime);
    if rows.is_empty() {
        ui.label(
            RichText::new("No tracked findings yet.")
                .small()
                .color(theme::TEXT_DIM),
        );
        return;
    }
    ScrollArea::vertical()
        .id_salt("tracked-findings")
        .max_height(210.0)
        .auto_shrink([false, true])
        .show_rows(ui, FINDING_ROW_HEIGHT, rows.len(), |ui, range| {
            for row in &rows[range] {
                let selected = runtime.selection().contains(&row.id);
                ui.horizontal(|ui| {
                    let visible = runtime.document().presentation().object_visible(row.id);
                    let eye = if visible { "●" } else { "○" };
                    if ui
                        .small_button(eye)
                        .on_hover_text("Toggle visibility")
                        .clicked()
                    {
                        if let Err(error) =
                            runtime.set_object_visibility_without_history(row.id, !visible)
                        {
                            actions.error = Some(error.to_string());
                        }
                    }
                    let response = ui.selectable_label(
                        selected,
                        RichText::new(format!("#{:04}  {}", row.ordinal, row.label)).size(12.0),
                    );
                    if response.clicked() {
                        let shift = ui.input(|input| input.modifiers.shift);
                        if shift {
                            runtime.toggle_selection(row.id);
                        } else {
                            runtime.select_only(row.id);
                        }
                    }
                    if response.double_clicked() {
                        actions.jump_to = Some(row.id);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("⌖")
                            .on_hover_text("Jump to finding")
                            .clicked()
                        {
                            actions.jump_to = Some(row.id);
                        }
                        ui.label(
                            RichText::new(&row.metric)
                                .monospace()
                                .small()
                                .color(theme::TEXT_DIM),
                        );
                    });
                });
            }
        });
}

fn show_layers(
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
    let external = runtime
        .document()
        .external_layers()
        .iter()
        .map(|layer| {
            (
                layer.id(),
                layer.name().to_owned(),
                layer.kind().clone(),
                layer.source_path().map(std::path::Path::to_path_buf),
                layer.source_object_count(),
                layer.class_mappings().clone(),
            )
        })
        .collect::<Vec<_>>();
    for (id, name, kind, source_path, count, mappings) in external {
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
                && sidecar_kind_for_external(&kind).is_some()
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
            let status = source_path.as_deref().map_or("embedded reference", |path| {
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
                    actions.status =
                        Some(format!("Removed source layer {name}; Undo restores it."));
                }
                Ok(false) => {}
                Err(error) => actions.error = Some(error.to_string()),
            }
            continue;
        }
        if load_layer {
            actions.load_sidecar = source_path.clone().zip(sidecar_kind_for_external(&kind));
        }
        let classes = runtime.external_classes(id);
        ui.indent(("external-controls", id), |ui| match classes {
            Err(error) => {
                ui.label(RichText::new(error.to_string()).small().color(theme::AMBER));
            }
            Ok(classes) if classes.is_empty() => {
                let message = if kind == dicom_viewer_core::ExternalLayerKind::Heatmap
                    && payload_loaded
                {
                    "Heatmap source result; export it through DICOM PM."
                } else {
                    "Source payload is unloaded or has no lossless editable objects."
                };
                ui.label(
                    RichText::new(message)
                        .small()
                        .color(theme::TEXT_DIM),
                );
            }
            Ok(classes) => {
                ui.collapsing("Class mapping & promotion", |ui| {
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
                    for class in &classes {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!(
                                "{} · {} {}",
                                class.label,
                                class.object_count,
                                class.geometry.label().to_lowercase()
                            ));
                            if !class.editable {
                                ui.label(RichText::new("read-only geometry").small().color(theme::AMBER));
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
                                    for (target_id, label, geometry) in &scheme_options {
                                        if *geometry != class.geometry {
                                            continue;
                                        }
                                        if ui
                                            .selectable_label(
                                                current.as_deref() == Some(target_id.as_str()),
                                                label,
                                            )
                                            .clicked()
                                        {
                                            if let Err(error) = runtime.set_external_class_mapping(
                                                id,
                                                &class.key,
                                                target_id,
                                            ) {
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
                                    if ui
                                        .small_button(format!("Use exact: {label}"))
                                        .clicked()
                                    {
                                        if let Err(error) = runtime.set_external_class_mapping(
                                            id,
                                            &class.key,
                                            suggested,
                                        ) {
                                            actions.error = Some(error.to_string());
                                        }
                                    }
                                }
                            }
                        });
                    }

                    let complete = classes.iter().all(|class| {
                        class.editable && mappings.contains_key(&class.key)
                    });
                    if ui
                        .add_enabled(complete, egui::Button::new("Make editable"))
                        .on_hover_text(
                            "Convert every source object only after every source class is mapped",
                        )
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

                    ui.collapsing("Source objects", |ui| match runtime.external_objects(id) {
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
                                            let enabled = object.promotable
                                                && mapped
                                                && !object.promoted;
                                            if ui
                                                .add_enabled(
                                                    enabled,
                                                    egui::Button::new(if object.promoted {
                                                        "Tracked"
                                                    } else {
                                                        "Promote"
                                                    }),
                                                )
                                                .clicked()
                                            {
                                                match runtime.promote_external_object(
                                                    id,
                                                    &object.source_object_id,
                                                ) {
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
                    });
                });
            }
        });
    }
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

fn show_inspector(
    ui: &mut egui::Ui,
    runtime: &mut WorkspaceRuntime,
    actions: &mut PathologyWorkspaceActions,
) {
    section_heading(ui, "Selection inspector");
    match runtime.selection().len() {
        0 => {
            ui.label(
                RichText::new("Select a finding or segment to inspect it.")
                    .small()
                    .color(theme::TEXT_DIM),
            );
        }
        count @ 2.. => {
            ui.label(format!("{count} objects selected"));
            actions.delete_selection = ui.button("Delete selected").clicked();
        }
        1 => {
            let id = *runtime
                .selection()
                .iter()
                .next()
                .expect("one selection exists");
            if let Some(row) = finding_rows(runtime).into_iter().find(|row| row.id == id) {
                let details = selected_details(runtime, id);
                ui.label(RichText::new(format!("#{:04} {}", row.ordinal, row.label)).strong());
                ui.label(
                    RichText::new(row.metric)
                        .monospace()
                        .color(theme::TEXT_MUTED),
                );
                if let Some(details) = details {
                    let compatible_classes = runtime
                        .document()
                        .scheme()
                        .classes()
                        .iter()
                        .filter(|class| class.geometry() == details.geometry)
                        .map(|class| (class.id().to_owned(), class.label().to_owned()))
                        .collect::<Vec<_>>();
                    egui::ComboBox::from_id_salt(("inspector-class", id))
                        .selected_text(
                            runtime
                                .document()
                                .scheme()
                                .class(&details.class_id)
                                .map_or(details.class_id.as_str(), |class| class.label()),
                        )
                        .show_ui(ui, |ui| {
                            for (class_id, label) in compatible_classes {
                                if ui
                                    .selectable_label(class_id == details.class_id, label)
                                    .clicked()
                                {
                                    if let Err(error) = runtime.reclassify_selection(&class_id) {
                                        actions.error = Some(error.to_string());
                                    }
                                }
                            }
                        });

                    let finding_sites = runtime.document().scheme().finding_sites().to_vec();
                    if !finding_sites.is_empty() {
                        let selected_site = details.finding_site.as_ref().and_then(|selected| {
                            finding_sites.iter().find(|site| selected.matches(site))
                        });
                        egui::ComboBox::from_id_salt(("inspector-finding-site", id))
                            .selected_text(
                                selected_site.map_or("No finding site", |site| site.meaning()),
                            )
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(
                                        details.finding_site.is_none(),
                                        "No finding site",
                                    )
                                    .clicked()
                                {
                                    if let Err(error) = runtime.set_selected_finding_site(None) {
                                        actions.error = Some(error.to_string());
                                    }
                                }
                                for site in &finding_sites {
                                    let selected = details
                                        .finding_site
                                        .as_ref()
                                        .is_some_and(|current| current.matches(site));
                                    if ui.selectable_label(selected, site.meaning()).clicked() {
                                        if let Err(error) =
                                            runtime.set_selected_finding_site(Some(site))
                                        {
                                            actions.error = Some(error.to_string());
                                        }
                                    }
                                }
                            });
                    }

                    let name_id = egui::Id::new(("finding-name", id));
                    let mut name = ui.ctx().data_mut(|data| {
                        data.get_temp::<String>(name_id)
                            .unwrap_or_else(|| details.name.clone().unwrap_or_default())
                    });
                    let name_response = ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .id(name_id)
                            .hint_text("Optional finding name"),
                    );
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(name_id, name.clone()));
                    if name_response.lost_focus() {
                        let value = (!name.trim().is_empty()).then_some(name.trim());
                        if let Err(error) = runtime.set_selected_name(value) {
                            actions.error = Some(error.to_string());
                        }
                    }

                    let comment_id = egui::Id::new(("finding-comment", id));
                    let mut comment = ui.ctx().data_mut(|data| {
                        data.get_temp::<String>(comment_id)
                            .unwrap_or_else(|| details.comment.clone().unwrap_or_default())
                    });
                    let comment_response = ui.add(
                        egui::TextEdit::multiline(&mut comment)
                            .id(comment_id)
                            .desired_rows(2)
                            .hint_text("Optional comment"),
                    );
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(comment_id, comment.clone()));
                    if comment_response.lost_focus() {
                        let value = (!comment.trim().is_empty()).then_some(comment.trim());
                        if let Err(error) = runtime.set_selected_comment(value) {
                            actions.error = Some(error.to_string());
                        }
                    }

                    ui.label(
                        RichText::new(format!("{} · {}", details.kind, details.source_status))
                            .small()
                            .color(theme::TEXT_MUTED),
                    );
                    ui.label(
                        RichText::new(format!(
                            "Tracking ID {}\nTracking UID {}",
                            details.tracking_id, details.tracking_uid
                        ))
                        .monospace()
                        .small()
                        .color(theme::TEXT_DIM),
                    );
                }
                ui.label(
                    RichText::new(format!("Object {id}"))
                        .monospace()
                        .small()
                        .color(theme::TEXT_DIM),
                );
                actions.delete_selection = ui.button("Delete finding").clicked();
            }
        }
    }
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

struct FindingRow {
    id: Uuid,
    ordinal: u64,
    label: String,
    metric: String,
}

struct SelectedDetails {
    class_id: String,
    geometry: AnnotationClassGeometry,
    name: Option<String>,
    comment: Option<String>,
    tracking_id: String,
    tracking_uid: String,
    kind: &'static str,
    source_status: &'static str,
    finding_site: Option<dicom_viewer_core::ControlledFindingSite>,
}

fn selected_details(runtime: &WorkspaceRuntime, id: Uuid) -> Option<SelectedDetails> {
    let document = runtime.document();
    if let Some(finding) = document.finding(id) {
        return Some(SelectedDetails {
            class_id: finding.class_id().to_owned(),
            geometry: match finding.geometry() {
                VectorFindingGeometry::Point(_) => AnnotationClassGeometry::Point,
                VectorFindingGeometry::Regions(_) => AnnotationClassGeometry::Region,
            },
            name: finding.name().map(str::to_owned),
            comment: finding.comment().map(str::to_owned),
            tracking_id: finding.tracking().id().to_owned(),
            tracking_uid: finding.tracking().uid().to_owned(),
            kind: "Vector finding",
            source_status: provenance_label(finding.provenance()),
            finding_site: finding.finding_site().cloned(),
        });
    }
    if let Some(segment) = document.segment(id) {
        return Some(SelectedDetails {
            class_id: segment.class_id().to_owned(),
            geometry: AnnotationClassGeometry::Region,
            name: segment.name().map(str::to_owned),
            comment: segment.comment().map(str::to_owned),
            tracking_id: segment.tracking().id().to_owned(),
            tracking_uid: segment.tracking().uid().to_owned(),
            kind: "Segmentation segment",
            source_status: provenance_label(segment.provenance()),
            finding_site: segment.finding_site().cloned(),
        });
    }
    let measurement = document.measurement(id)?;
    Some(SelectedDetails {
        class_id: measurement.class_id().to_owned(),
        geometry: AnnotationClassGeometry::Region,
        name: measurement.name().map(str::to_owned),
        comment: measurement.comment().map(str::to_owned),
        tracking_id: measurement.tracking().id().to_owned(),
        tracking_uid: measurement.tracking().uid().to_owned(),
        kind: "Linear measurement",
        source_status: provenance_label(measurement.provenance()),
        finding_site: measurement.finding_site().cloned(),
    })
}

fn provenance_label(provenance: &dicom_viewer_core::WorkspaceObjectProvenance) -> &'static str {
    match provenance {
        dicom_viewer_core::WorkspaceObjectProvenance::Manual => "manual",
        dicom_viewer_core::WorkspaceObjectProvenance::Promoted { .. } => "promoted",
    }
}

fn finding_rows(runtime: &WorkspaceRuntime) -> Vec<FindingRow> {
    let document = runtime.document();
    let mut rows = document
        .vector_findings()
        .map(|finding| FindingRow {
            id: finding.object_id(),
            ordinal: finding.ordinal(),
            label: document.scheme().class(finding.class_id()).map_or_else(
                || finding.class_id().to_owned(),
                |class| class.label().to_owned(),
            ),
            metric: match finding.geometry() {
                VectorFindingGeometry::Point(_) => "point".into(),
                VectorFindingGeometry::Regions(components) => {
                    format!(
                        "{} region{}",
                        components.len(),
                        if components.len() == 1 { "" } else { "s" }
                    )
                }
            },
        })
        .chain(document.segments().map(|segment| {
            let geometry = document.composite_segment(segment.object_id()).ok();
            FindingRow {
                id: segment.object_id(),
                ordinal: segment.ordinal(),
                label: document.scheme().class(segment.class_id()).map_or_else(
                    || segment.class_id().to_owned(),
                    |class| class.label().to_owned(),
                ),
                metric: format!(
                    "{} component{}",
                    geometry
                        .as_ref()
                        .map_or(0, |geometry| geometry.components().len()),
                    if geometry
                        .as_ref()
                        .is_some_and(|geometry| geometry.components().len() == 1)
                    {
                        ""
                    } else {
                        "s"
                    }
                ),
            }
        }))
        .chain(
            document
                .measurements()
                .iter()
                .map(|measurement| FindingRow {
                    id: measurement.object_id(),
                    ordinal: measurement.ordinal(),
                    label: document.scheme().class(measurement.class_id()).map_or_else(
                        || "Ruler".into(),
                        |class| format!("{} length", class.label()),
                    ),
                    metric: measurement.physical_length_mm().map_or_else(
                        || "unscaled".into(),
                        |length| {
                            if length >= 1.0 {
                                format!("{length:.3} mm")
                            } else {
                                format!("{:.1} µm", length * 1_000.0)
                            }
                        },
                    ),
                }),
        )
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row.ordinal);
    rows
}

fn visible_palette(runtime: &WorkspaceRuntime) -> Vec<(String, String, [u8; 3], bool)> {
    let expected_geometry = match runtime.active_tool() {
        ActiveTool::Point => Some(AnnotationClassGeometry::Point),
        ActiveTool::Polygon | ActiveTool::Brush | ActiveTool::Ruler => {
            Some(AnnotationClassGeometry::Region)
        }
        ActiveTool::Pan | ActiveTool::Select => None,
    };
    runtime
        .document()
        .scheme()
        .classes()
        .iter()
        .filter(|class| expected_geometry.is_none_or(|geometry| class.geometry() == geometry))
        .map(|class| {
            (
                class.id().to_owned(),
                class.label().to_owned(),
                class.display_color(),
                runtime.active_class_id() == class.id(),
            )
        })
        .collect()
}

fn tool_glyph(tool: ActiveTool) -> &'static str {
    match tool {
        ActiveTool::Pan => "✥",
        ActiveTool::Select => "⌁",
        ActiveTool::Polygon => "△",
        ActiveTool::Brush => "●",
        ActiveTool::Point => "+",
        ActiveTool::Ruler => "╱",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::run_ui;
    use dicom_viewer_core::{AnnotationScheme, ViewerSourceIdentity};

    fn runtime() -> WorkspaceRuntime {
        WorkspaceRuntime::new(
            ViewerSourceIdentity::new(1, 0, 0, 0, 0, 0, (1_000, 1_000)),
            AnnotationScheme::general_pathology_v1(),
        )
        .unwrap()
    }

    #[test]
    fn pathology_workspace_renders_as_one_dense_panel_and_tool_rail() {
        let mut runtime = runtime();
        let output = run_ui(|ui| {
            let _ = show_tool_rail(ui, &mut runtime);
            show_pathology_workspace_panel(ui, &mut runtime);
        });
        assert!(!output.shapes.is_empty());
    }

    #[test]
    fn common_palette_filters_classes_by_tool_geometry() {
        let mut runtime = runtime();
        runtime.set_active_tool(ActiveTool::Point).unwrap();
        let point_classes = visible_palette(&runtime);
        assert_eq!(
            point_classes
                .iter()
                .map(|class| class.0.as_str())
                .collect::<Vec<_>>(),
            vec!["cell", "nucleus"]
        );
        runtime.set_active_tool(ActiveTool::Polygon).unwrap();
        assert!(visible_palette(&runtime)
            .iter()
            .all(|class| !matches!(class.0.as_str(), "cell" | "nucleus")));
    }
}
