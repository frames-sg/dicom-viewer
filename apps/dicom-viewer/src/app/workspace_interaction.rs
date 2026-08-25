use dicom_viewer_core::{Point2, StudySummary};
use eframe::egui::{self, Rect};

use super::camera::CameraView;
use super::viewport::{base_contains_point, screen_to_base};
use super::workspace::{ActiveTool, ToolTransitionOutcome};
use super::DicomViewerApp;

#[derive(Debug, Default)]
pub(super) struct WorkspaceCanvasInteraction {
    pub(super) drag_consumed: bool,
    pub(super) click_consumed: bool,
    pub(super) pan_requested: bool,
}

impl DicomViewerApp {
    pub(super) fn handle_workspace_interaction(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
        summary: &StudySummary,
        view: CameraView,
        accepts_keys: bool,
    ) -> WorkspaceCanvasInteraction {
        let mut interaction = WorkspaceCanvasInteraction::default();
        let Some(runtime) = self.workspace.as_mut() else {
            return interaction;
        };

        if accepts_keys {
            let (modifiers, pressed) = ui.input(|input| {
                let pressed = |key| input.key_pressed(key);
                (
                    input.modifiers,
                    [
                        pressed(egui::Key::V),
                        pressed(egui::Key::P),
                        pressed(egui::Key::B),
                        pressed(egui::Key::K),
                        pressed(egui::Key::R),
                        pressed(egui::Key::N),
                        pressed(egui::Key::Enter),
                        pressed(egui::Key::Escape),
                        pressed(egui::Key::Delete),
                        pressed(egui::Key::Z),
                        pressed(egui::Key::Y),
                        pressed(egui::Key::OpenBracket),
                        pressed(egui::Key::CloseBracket),
                    ],
                )
            });
            let command_handled = if modifiers.command && pressed[9] {
                if modifiers.shift {
                    if runtime.redo() {
                        self.status = "Redid the last pathology command.".into();
                    }
                } else if runtime.undo_draft_cancel() {
                    self.status = "Restored the last cancelled draft step.".into();
                } else if runtime.undo() {
                    self.status = "Undid the last pathology command.".into();
                }
                true
            } else if modifiers.ctrl && pressed[10] {
                if runtime.redo() {
                    self.status = "Redid the last pathology command.".into();
                }
                true
            } else {
                false
            };
            if !command_handled && !modifiers.command && !modifiers.ctrl && !modifiers.alt {
                let requested_tool = if pressed[0] {
                    Some(ActiveTool::Select)
                } else if pressed[1] {
                    Some(ActiveTool::Polygon)
                } else if pressed[2] {
                    Some(ActiveTool::Brush)
                } else if pressed[3] {
                    Some(ActiveTool::Point)
                } else if pressed[4] {
                    Some(ActiveTool::Ruler)
                } else {
                    None
                };
                if let Some(tool) = requested_tool {
                    if tool == ActiveTool::Brush {
                        if let Err(error) = runtime.ensure_segmentation_layer() {
                            self.status = error.to_string();
                        }
                    }
                    if runtime.request_tool(tool) == ToolTransitionOutcome::BlockedByDraft {
                        self.status =
                            "Unfinished polygon: choose Resume, Finish, or Discard.".into();
                    }
                }
                if pressed[5] {
                    runtime.begin_new_segment();
                    self.status = "The next edit will create an independent tracked object.".into();
                }
                if pressed[6] && runtime.draft().is_some() {
                    match runtime.finish_draft() {
                        Ok(_) => self.status = "Finished polygon.".into(),
                        Err(error) => self.status = error.to_string(),
                    }
                }
                if pressed[7] {
                    if runtime.brush_stroke().is_some() {
                        runtime.cancel_pointer_interaction();
                        self.status = "Cancelled the current brush stroke.".into();
                    } else if runtime.cancel_draft_step() {
                        self.status = "Removed the latest draft vertex; Escape again to continue undoing the draft.".into();
                    } else if runtime.cancel_ruler() {
                        self.status = "Cancelled the unfinished ruler.".into();
                    }
                }
                if pressed[8] {
                    match runtime.delete_selection() {
                        Ok(count) if count > 0 => {
                            self.status = format!("Deleted {count} tracked object(s).")
                        }
                        Ok(_) => {}
                        Err(error) => self.status = error.to_string(),
                    }
                }
                if pressed[11] {
                    runtime.adjust_brush_diameter(0.8);
                }
                if pressed[12] {
                    runtime.adjust_brush_diameter(1.25);
                }
            }
        }

        let space_pan = accepts_keys && ui.input(|input| input.key_down(egui::Key::Space));
        if runtime.active_tool() == ActiveTool::Pan || space_pan {
            interaction.pan_requested = response.dragged();
            interaction.drag_consumed = interaction.pan_requested;
            return interaction;
        }

        let point = response
            .interact_pointer_pos()
            .map(|pointer| screen_to_base(rect, pointer, view.center_base, view.zoom))
            .filter(|point| base_contains_point(summary, *point))
            .map(|point| Point2::new(f64::from(point.x), f64::from(point.y)));
        let alt = ui.input(|input| input.modifiers.alt);

        match runtime.active_tool() {
            ActiveTool::Pan => unreachable!(),
            ActiveTool::Select => {
                if response.drag_started() {
                    if let Some(point) = point {
                        let tolerance = 11.0 / f64::from(view.zoom.max(f32::EPSILON));
                        interaction.drag_consumed = runtime.begin_handle_drag(point, tolerance);
                    }
                }
                if runtime.handle_drag_active() && response.dragged() {
                    interaction.drag_consumed = true;
                    if let Some(point) = point {
                        let mpp = valid_mpp(summary);
                        if let Err(error) = runtime.update_handle_drag(point, |start, end| {
                            mpp.map(|(mpp_x, mpp_y)| {
                                ((end.x - start.x) * mpp_x).hypot((end.y - start.y) * mpp_y)
                                    / 1_000.0
                            })
                        }) {
                            self.status = format!("Geometry handle cannot move there: {error}");
                        }
                    }
                }
                if runtime.handle_drag_active() && response.drag_stopped() {
                    interaction.drag_consumed = true;
                    if runtime.finish_handle_drag() {
                        self.status = "Committed one geometry-move command.".into();
                    }
                } else if runtime.handle_drag_active()
                    && (!ui.input(|input| input.pointer.primary_down())
                        || ui.input(|input| input.viewport().focused == Some(false)))
                {
                    runtime.cancel_handle_drag();
                    self.status =
                        "Pointer capture was lost; the geometry move was cancelled.".into();
                }
                if response.clicked() && !interaction.drag_consumed {
                    interaction.click_consumed = true;
                    if let Some(point) = point {
                        let tolerance = 9.0 / f64::from(view.zoom.max(f32::EPSILON));
                        if let Some(id) = runtime.hit_test(point, tolerance) {
                            if ui.input(|input| input.modifiers.shift) {
                                runtime.toggle_selection(id);
                            } else {
                                runtime.select_only(id);
                            }
                        } else if !ui.input(|input| input.modifiers.shift) {
                            runtime.clear_selection();
                        }
                    }
                }
            }
            ActiveTool::Polygon => {
                if response.double_clicked() {
                    interaction.click_consumed = true;
                    if let Some(point) = point {
                        let original = runtime.segment_operation();
                        if alt {
                            runtime.set_segment_operation(original.reversed());
                        }
                        let add = runtime.add_polygon_point(point);
                        runtime.set_segment_operation(original);
                        match add.and_then(|()| runtime.finish_draft().map(|_| ())) {
                            Ok(()) => self.status = "Finished polygon.".into(),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                } else if response.clicked() {
                    interaction.click_consumed = true;
                    if let Some(point) = point {
                        let original = runtime.segment_operation();
                        if alt {
                            runtime.set_segment_operation(original.reversed());
                        }
                        let result = runtime.add_polygon_point(point);
                        runtime.set_segment_operation(original);
                        self.status = match result {
                            Ok(()) => {
                                "Polygon vertex added; Enter or double-click to finish.".into()
                            }
                            Err(error) => error.to_string(),
                        };
                    }
                }
            }
            ActiveTool::Brush => {
                if response.drag_started() {
                    interaction.drag_consumed = true;
                    if let Some(point) = point {
                        let operation = if alt {
                            runtime.segment_operation().reversed()
                        } else {
                            runtime.segment_operation()
                        };
                        if let Err(error) =
                            runtime.begin_brush_stroke_with_operation(point, operation)
                        {
                            self.status = error.to_string();
                        }
                    }
                }
                if response.dragged() {
                    interaction.drag_consumed = true;
                    if let Some(point) = point {
                        runtime.extend_brush_stroke(point);
                    }
                }
                if response.drag_stopped() {
                    interaction.drag_consumed = true;
                    match runtime.finish_brush_stroke() {
                        Ok(Some(_)) => self.status = "Committed one brush-stroke command.".into(),
                        Ok(None) => {
                            self.status =
                                "Erase did not intersect the selected segment; nothing changed."
                                    .into()
                        }
                        Err(error) => self.status = error.to_string(),
                    }
                } else if runtime.brush_stroke().is_some()
                    && (!ui.input(|input| input.pointer.primary_down())
                        || ui.input(|input| input.viewport().focused == Some(false)))
                {
                    runtime.cancel_pointer_interaction();
                    self.status =
                        "Pointer capture was lost; the brush stroke was cancelled.".into();
                }
            }
            ActiveTool::Point => {
                if response.clicked() {
                    interaction.click_consumed = true;
                    if let Some(point) = point {
                        match runtime.add_point_finding(point) {
                            Ok(_) => self.status = "Added an independent tracked point.".into(),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                }
            }
            ActiveTool::Ruler => {
                if response.clicked() {
                    interaction.click_consumed = true;
                    if let Some(point) = point {
                        let mpp = valid_mpp(summary);
                        match runtime.place_ruler_point(point, |start, end| {
                            mpp.map(|(mpp_x, mpp_y)| {
                                ((end.x - start.x) * mpp_x).hypot((end.y - start.y) * mpp_y)
                                    / 1_000.0
                            })
                        }) {
                            Ok(Some(id)) => {
                                let label = runtime
                                    .document()
                                    .measurement(id)
                                    .and_then(|measurement| measurement.physical_length_mm())
                                    .map_or_else(
                                        || "unscaled pixels".into(),
                                        |millimeters| {
                                            if millimeters >= 1.0 {
                                                format!("{millimeters:.3} mm")
                                            } else {
                                                format!("{:.1} µm", millimeters * 1_000.0)
                                            }
                                        },
                                    );
                                self.status = format!("Added tracked ruler: {label}.");
                            }
                            Ok(None) => self.status = "Ruler start set.".into(),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                }
            }
        }
        interaction
    }
}

fn valid_mpp(summary: &StudySummary) -> Option<(f64, f64)> {
    let (x, y) = summary.mpp?;
    (x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0).then_some((x, y))
}
