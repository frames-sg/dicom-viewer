use super::*;

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
                        .add_sized([40.0, 34.0], egui::Button::selectable(selected, ""))
                        .on_hover_text(format!("{} ({})", tool.label(), tool.shortcut()));
                    let icon_color = if selected {
                        theme::CANVAS_EDGE
                    } else if response.hovered() {
                        theme::AMBER_BRIGHT
                    } else {
                        theme::TEXT
                    };
                    ui.painter().extend(tool_icon_shapes(
                        tool,
                        response.rect.shrink(7.0),
                        icon_color,
                    ));
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

fn tool_icon_shapes(tool: ActiveTool, rect: egui::Rect, color: Color32) -> Vec<egui::Shape> {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.42;
    let stroke = Stroke::new(1.8, color);
    let point = |x: f32, y: f32| center + egui::vec2(x * radius, y * radius);

    match tool {
        ActiveTool::Pan => vec![
            egui::Shape::line_segment([point(-1.0, 0.0), point(1.0, 0.0)], stroke),
            egui::Shape::line_segment([point(0.0, -1.0), point(0.0, 1.0)], stroke),
            egui::Shape::line_segment([point(-1.0, 0.0), point(-0.65, -0.28)], stroke),
            egui::Shape::line_segment([point(-1.0, 0.0), point(-0.65, 0.28)], stroke),
            egui::Shape::line_segment([point(1.0, 0.0), point(0.65, -0.28)], stroke),
            egui::Shape::line_segment([point(1.0, 0.0), point(0.65, 0.28)], stroke),
            egui::Shape::line_segment([point(0.0, -1.0), point(-0.28, -0.65)], stroke),
            egui::Shape::line_segment([point(0.0, -1.0), point(0.28, -0.65)], stroke),
            egui::Shape::line_segment([point(0.0, 1.0), point(-0.28, 0.65)], stroke),
            egui::Shape::line_segment([point(0.0, 1.0), point(0.28, 0.65)], stroke),
        ],
        ActiveTool::Select => vec![egui::Shape::closed_line(
            vec![
                point(-0.78, -0.92),
                point(-0.66, 0.78),
                point(-0.18, 0.31),
                point(0.29, 0.95),
                point(0.65, 0.69),
                point(0.19, 0.08),
                point(0.83, -0.02),
            ],
            stroke,
        )],
        ActiveTool::Polygon => vec![egui::Shape::closed_line(
            vec![point(0.0, -0.9), point(0.9, 0.75), point(-0.9, 0.75)],
            stroke,
        )],
        ActiveTool::Brush => vec![egui::Shape::circle_filled(center, radius * 0.66, color)],
        ActiveTool::Point => vec![
            egui::Shape::line_segment([point(-0.9, 0.0), point(0.9, 0.0)], stroke),
            egui::Shape::line_segment([point(0.0, -0.9), point(0.0, 0.9)], stroke),
            egui::Shape::circle_filled(center, radius * 0.16, color),
        ],
        ActiveTool::Ruler => vec![
            egui::Shape::line_segment([point(-0.75, 0.75), point(0.75, -0.75)], stroke),
            egui::Shape::line_segment([point(-0.98, 0.5), point(-0.5, 0.98)], stroke),
            egui::Shape::line_segment([point(0.5, -0.98), point(0.98, -0.5)], stroke),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_icon_is_vector_geometry_not_font_text() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(24.0, 24.0));

        for tool in ActiveTool::ALL {
            let shapes = tool_icon_shapes(tool, rect, theme::TEXT);
            assert!(!shapes.is_empty(), "{} needs a visible icon", tool.label());
            assert!(shapes
                .iter()
                .all(|shape| !matches!(shape, egui::Shape::Text(_))));
        }
    }
}
