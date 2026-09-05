use super::WorkspaceCanvasInteraction;
use crate::app::workspace::{ActiveTool, WorkspaceRuntime};
use dicom_viewer_core::Point2;

/// One pointer observation, translated from egui before issuing domain commands.
#[derive(Debug, Default)]
pub(super) struct PointerInput {
    pub(super) point: Option<Point2>,
    pub(super) clicked: bool,
    pub(super) double_clicked: bool,
    pub(super) drag_started: bool,
    pub(super) dragged: bool,
    pub(super) drag_stopped: bool,
    pub(super) capture_lost: bool,
    pub(super) shift: bool,
    pub(super) alt: bool,
    pub(super) zoom: f32,
    pub(super) mpp: Option<(f64, f64)>,
}

pub(super) fn handle_pointer(
    runtime: &mut WorkspaceRuntime,
    status: &mut String,
    input: &PointerInput,
) -> WorkspaceCanvasInteraction {
    let mut interaction = WorkspaceCanvasInteraction::default();
    match runtime.active_tool() {
        ActiveTool::Pan => {}
        ActiveTool::Select => handle_select(runtime, status, input, &mut interaction),
        ActiveTool::Polygon => handle_polygon(runtime, status, input, &mut interaction),
        ActiveTool::Brush => handle_brush(runtime, status, input, &mut interaction),
        ActiveTool::Point => handle_point(runtime, status, input, &mut interaction),
        ActiveTool::Ruler => handle_ruler(runtime, status, input, &mut interaction),
    }
    interaction
}

fn handle_select(
    runtime: &mut WorkspaceRuntime,
    status: &mut String,
    input: &PointerInput,
    interaction: &mut WorkspaceCanvasInteraction,
) {
    let point = input.point;
    if input.drag_started {
        if let Some(point) = point {
            let tolerance = 11.0 / f64::from(input.zoom.max(f32::EPSILON));
            interaction.drag_consumed = runtime.begin_handle_drag(point, tolerance);
        }
    }
    if runtime.handle_drag_active() && input.dragged {
        interaction.drag_consumed = true;
        if let Some(point) = point {
            let mpp = input.mpp;
            if let Err(error) = runtime.update_handle_drag(point, |start, end| {
                mpp.map(|(mpp_x, mpp_y)| {
                    ((end.x - start.x) * mpp_x).hypot((end.y - start.y) * mpp_y) / 1_000.0
                })
            }) {
                *status = format!("Geometry handle cannot move there: {error}");
            }
        }
    }
    if runtime.handle_drag_active() && input.drag_stopped {
        interaction.drag_consumed = true;
        if runtime.finish_handle_drag() {
            *status = "Committed one geometry-move command.".into();
        }
    } else if runtime.handle_drag_active() && input.capture_lost {
        runtime.cancel_handle_drag();
        *status = "Pointer capture was lost; the geometry move was cancelled.".into();
    }
    if input.clicked && !interaction.drag_consumed {
        interaction.click_consumed = true;
        if let Some(point) = point {
            let tolerance = 9.0 / f64::from(input.zoom.max(f32::EPSILON));
            if let Some(id) = runtime.hit_test(point, tolerance) {
                if input.shift {
                    runtime.toggle_selection(id);
                } else {
                    runtime.select_only(id);
                }
            } else if !input.shift {
                runtime.clear_selection();
            }
        }
    }
}

fn handle_polygon(
    runtime: &mut WorkspaceRuntime,
    status: &mut String,
    input: &PointerInput,
    interaction: &mut WorkspaceCanvasInteraction,
) {
    let point = input.point;
    let alt = input.alt;
    if input.double_clicked {
        interaction.click_consumed = true;
        if let Some(point) = point {
            let original = runtime.segment_operation();
            if alt {
                runtime.set_segment_operation(original.reversed());
            }
            let add = runtime.add_polygon_point(point);
            runtime.set_segment_operation(original);
            match add.and_then(|()| runtime.finish_draft().map(|_| ())) {
                Ok(()) => *status = "Finished polygon.".into(),
                Err(error) => *status = error.to_string(),
            }
        }
    } else if input.clicked {
        interaction.click_consumed = true;
        if let Some(point) = point {
            let original = runtime.segment_operation();
            if alt {
                runtime.set_segment_operation(original.reversed());
            }
            let result = runtime.add_polygon_point(point);
            runtime.set_segment_operation(original);
            *status = match result {
                Ok(()) => "Polygon vertex added; Enter or double-click to finish.".into(),
                Err(error) => error.to_string(),
            };
        }
    }
}

fn handle_brush(
    runtime: &mut WorkspaceRuntime,
    status: &mut String,
    input: &PointerInput,
    interaction: &mut WorkspaceCanvasInteraction,
) {
    let point = input.point;
    let alt = input.alt;
    if input.drag_started {
        interaction.drag_consumed = true;
        if let Some(point) = point {
            let operation = if alt {
                runtime.segment_operation().reversed()
            } else {
                runtime.segment_operation()
            };
            if let Err(error) = runtime.begin_brush_stroke_with_operation(point, operation) {
                *status = error.to_string();
            }
        }
    }
    if input.dragged {
        interaction.drag_consumed = true;
        if let Some(point) = point {
            runtime.extend_brush_stroke(point);
        }
    }
    if input.drag_stopped {
        interaction.drag_consumed = true;
        match runtime.finish_brush_stroke() {
            Ok(Some(_)) => *status = "Committed one brush-stroke command.".into(),
            Ok(None) => {
                *status = "Erase did not intersect the selected segment; nothing changed.".into()
            }
            Err(error) => *status = error.to_string(),
        }
    } else if runtime.brush_stroke().is_some() && input.capture_lost {
        runtime.cancel_pointer_interaction();
        *status = "Pointer capture was lost; the brush stroke was cancelled.".into();
    }
}

fn handle_point(
    runtime: &mut WorkspaceRuntime,
    status: &mut String,
    input: &PointerInput,
    interaction: &mut WorkspaceCanvasInteraction,
) {
    let point = input.point;
    if input.clicked {
        interaction.click_consumed = true;
        if let Some(point) = point {
            match runtime.add_point_finding(point) {
                Ok(_) => *status = "Added an independent tracked point.".into(),
                Err(error) => *status = error.to_string(),
            }
        }
    }
}

fn handle_ruler(
    runtime: &mut WorkspaceRuntime,
    status: &mut String,
    input: &PointerInput,
    interaction: &mut WorkspaceCanvasInteraction,
) {
    let point = input.point;
    if input.clicked {
        interaction.click_consumed = true;
        if let Some(point) = point {
            let mpp = input.mpp;
            match runtime.place_ruler_point(point, |start, end| {
                mpp.map(|(mpp_x, mpp_y)| {
                    ((end.x - start.x) * mpp_x).hypot((end.y - start.y) * mpp_y) / 1_000.0
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
                    *status = format!("Added tracked ruler: {label}.");
                }
                Ok(None) => *status = "Ruler start set.".into(),
                Err(error) => *status = error.to_string(),
            }
        }
    }
}
