mod keyboard;
mod pointer;

#[cfg(test)]
mod tests;

use dicom_viewer_core::{Point2, StudySummary};
use eframe::egui::{self, Rect};

use super::camera::CameraView;
use super::viewport::{base_contains_point, screen_to_base};
use super::workspace::ActiveTool;
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
            ui.input(|input| keyboard::handle_keyboard(runtime, &mut self.status, input));
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
        let input = ui.input(|input| pointer::PointerInput {
            point,
            clicked: response.clicked(),
            double_clicked: response.double_clicked(),
            drag_started: response.drag_started(),
            dragged: response.dragged(),
            drag_stopped: response.drag_stopped(),
            capture_lost: !input.pointer.primary_down() || input.viewport().focused == Some(false),
            shift: input.modifiers.shift,
            alt: input.modifiers.alt,
            zoom: view.zoom,
            mpp: valid_mpp(summary),
        });
        pointer::handle_pointer(runtime, &mut self.status, &input)
    }
}

fn valid_mpp(summary: &StudySummary) -> Option<(f64, f64)> {
    let (x, y) = summary.mpp?;
    (x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0).then_some((x, y))
}
