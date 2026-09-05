use eframe::egui;

use crate::app::workspace::{ActiveTool, ToolTransitionOutcome, WorkspaceRuntime};

pub(super) fn handle_keyboard(
    runtime: &mut WorkspaceRuntime,
    status: &mut String,
    input: &egui::InputState,
) {
    let modifiers = input.modifiers;
    if !handle_history_key(runtime, status, input)
        && !modifiers.command
        && !modifiers.ctrl
        && !modifiers.alt
    {
        handle_edit_keys(runtime, status, input);
    }
}

fn handle_history_key(
    runtime: &mut WorkspaceRuntime,
    status: &mut String,
    input: &egui::InputState,
) -> bool {
    let modifiers = input.modifiers;
    if modifiers.command && input.key_pressed(egui::Key::Z) {
        if modifiers.shift {
            if runtime.redo() {
                *status = "Redid the last pathology command.".into();
            }
        } else if runtime.undo_draft_cancel() {
            *status = "Restored the last cancelled draft step.".into();
        } else if runtime.undo() {
            *status = "Undid the last pathology command.".into();
        }
        true
    } else if modifiers.ctrl && input.key_pressed(egui::Key::Y) {
        if runtime.redo() {
            *status = "Redid the last pathology command.".into();
        }
        true
    } else {
        false
    }
}

fn requested_tool(input: &egui::InputState) -> Option<ActiveTool> {
    if input.key_pressed(egui::Key::V) {
        Some(ActiveTool::Select)
    } else if input.key_pressed(egui::Key::P) {
        Some(ActiveTool::Polygon)
    } else if input.key_pressed(egui::Key::B) {
        Some(ActiveTool::Brush)
    } else if input.key_pressed(egui::Key::K) {
        Some(ActiveTool::Point)
    } else if input.key_pressed(egui::Key::R) {
        Some(ActiveTool::Ruler)
    } else {
        None
    }
}

fn handle_edit_keys(runtime: &mut WorkspaceRuntime, status: &mut String, input: &egui::InputState) {
    if let Some(tool) = requested_tool(input) {
        if tool == ActiveTool::Brush {
            if let Err(error) = runtime.ensure_segmentation_layer() {
                *status = error.to_string();
            }
        }
        if runtime.request_tool(tool) == ToolTransitionOutcome::BlockedByDraft {
            *status = "Unfinished polygon: choose Resume, Finish, or Discard.".into();
        }
    }
    if input.key_pressed(egui::Key::N) {
        runtime.begin_new_segment();
        *status = "The next edit will create an independent tracked object.".into();
    }
    if input.key_pressed(egui::Key::Enter) && runtime.draft().is_some() {
        match runtime.finish_draft() {
            Ok(_) => *status = "Finished polygon.".into(),
            Err(error) => *status = error.to_string(),
        }
    }
    if input.key_pressed(egui::Key::Escape) {
        if runtime.brush_stroke().is_some() {
            runtime.cancel_pointer_interaction();
            *status = "Cancelled the current brush stroke.".into();
        } else if runtime.cancel_draft_step() {
            *status =
                "Removed the latest draft vertex; Escape again to continue undoing the draft."
                    .into();
        } else if runtime.cancel_ruler() {
            *status = "Cancelled the unfinished ruler.".into();
        }
    }
    if input.key_pressed(egui::Key::Delete) {
        match runtime.delete_selection() {
            Ok(count) if count > 0 => *status = format!("Deleted {count} tracked object(s)."),
            Ok(_) => {}
            Err(error) => *status = error.to_string(),
        }
    }
    if input.key_pressed(egui::Key::OpenBracket) {
        runtime.adjust_brush_diameter(0.8);
    }
    if input.key_pressed(egui::Key::CloseBracket) {
        runtime.adjust_brush_diameter(1.25);
    }
}
