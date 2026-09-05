use super::*;

pub(super) fn show_inspector(
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
    let object = runtime.document().object(id)?;
    let (geometry, kind) = match object.geometry_kind() {
        WorkspaceObjectGeometryKind::Point => (AnnotationClassGeometry::Point, "Vector finding"),
        WorkspaceObjectGeometryKind::Region => (AnnotationClassGeometry::Region, "Vector finding"),
        WorkspaceObjectGeometryKind::Segmentation => {
            (AnnotationClassGeometry::Region, "Segmentation segment")
        }
        WorkspaceObjectGeometryKind::Measurement => {
            (AnnotationClassGeometry::Region, "Linear measurement")
        }
    };
    Some(SelectedDetails {
        class_id: object.class_id().to_owned(),
        geometry,
        name: object.name().map(str::to_owned),
        comment: object.comment().map(str::to_owned),
        tracking_id: object.tracking().id().to_owned(),
        tracking_uid: object.tracking().uid().to_owned(),
        kind,
        source_status: provenance_label(object.provenance()),
        finding_site: object.finding_site().cloned(),
    })
}

fn provenance_label(provenance: &dicom_viewer_core::WorkspaceObjectProvenance) -> &'static str {
    match provenance {
        dicom_viewer_core::WorkspaceObjectProvenance::Manual => "manual",
        dicom_viewer_core::WorkspaceObjectProvenance::Promoted { .. } => "promoted",
    }
}
