use super::*;
use dicom_viewer_core::VectorFindingGeometry;

pub(super) fn show_findings(
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

pub(super) struct FindingRow {
    pub(super) id: Uuid,
    pub(super) ordinal: u64,
    pub(super) label: String,
    pub(super) metric: String,
}

pub(super) fn finding_rows(runtime: &WorkspaceRuntime) -> Vec<FindingRow> {
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
