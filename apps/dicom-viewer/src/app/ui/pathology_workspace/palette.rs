use super::*;

pub(super) fn show_scheme_and_palette(
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

pub(super) fn visible_palette(runtime: &WorkspaceRuntime) -> Vec<(String, String, [u8; 3], bool)> {
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
