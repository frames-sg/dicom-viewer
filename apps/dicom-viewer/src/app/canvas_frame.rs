use dicom_viewer_core::ViewerStudy;
use eframe::egui::{self, Frame, Rect, Sense};
use std::sync::Arc;

use super::camera::{raw_wheel_delta_y, wheel_zoom_factor, CameraFrame, CameraView};
use super::ui::chrome::ToolbarActions;
use super::ui::overlay::{
    draw_canvas_overlays, paint_canvas_background, paint_empty_state, OverlayInfo,
};
use super::viewport::screen_to_base;
use super::viewport_export::should_draw_canvas_hud;
use super::workspace::{draw_external_layer_overlays, draw_workspace_overlay};
use super::workspace_interaction::WorkspaceCanvasInteraction;
use super::{theme, DicomViewerApp};

impl DicomViewerApp {
    pub(super) fn show_canvas(
        &mut self,
        ui: &mut egui::Ui,
        study: Option<&Arc<ViewerStudy>>,
        opening: bool,
        actions: &ToolbarActions,
        stable_dt: f32,
    ) {
        egui::CentralPanel::default_margins()
            .frame(Frame::NONE.fill(theme::CANVAS))
            .show_inside(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                self.last_canvas_rect = Some(rect);
                if let Some(pending) = &mut self.pending_viewport_export {
                    pending.update_canvas(rect, ui.ctx().viewport_rect());
                }
                let response = ui.allocate_rect(rect, Sense::click_and_drag());
                if response.clicked() || response.drag_started() {
                    response.request_focus();
                } else if ui.input(|input| input.pointer.any_pressed()) && !response.hovered() {
                    response.surrender_focus();
                }
                let painter = ui.painter_at(rect);
                paint_canvas_background(&painter, rect);

                let Some(study) = study else {
                    paint_empty_state(&painter, rect, opening);
                    return;
                };

                let camera_frame =
                    self.prepare_canvas_camera(ui, &response, rect, study, actions, stable_dt);
                self.canvas.paint(
                    ui.ctx(),
                    &painter,
                    rect,
                    study,
                    self.active_generation,
                    camera_frame,
                );
                if let Some(runtime) = &mut self.workspace {
                    if let Err(error) = runtime.refresh_spatial_index() {
                        self.status =
                            format!("Could not update annotation viewport index: {error}");
                    }
                }
                if let Some((count, reason)) = self.canvas.cpu_fallback() {
                    if count > self.reported_cpu_fallbacks {
                        self.reported_cpu_fallbacks = count;
                        self.status = format!(
                            "{} preferred → wgpu; CPU fallback used for {count} tile(s): {reason}",
                            study.summary().tile_decode_backend
                        );
                    }
                }

                self.paint_annotation_overlays(
                    &painter,
                    rect,
                    study,
                    &response,
                    camera_frame.rendered,
                );
            });
    }

    fn prepare_canvas_camera(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
        study: &ViewerStudy,
        actions: &ToolbarActions,
        stable_dt: f32,
    ) -> CameraFrame {
        if actions.fit {
            self.canvas.record_zoom_input();
            self.camera.request_fit();
        }
        self.camera.prepare_canvas(rect, study.summary());
        if actions.zoom_out {
            self.canvas.record_zoom_input();
            self.camera.zoom_about_center(rect, 0.8);
        }
        if actions.zoom_in {
            self.canvas.record_zoom_input();
            self.camera.zoom_about_center(rect, 1.25);
        }

        let accepts_keys =
            (response.hovered() || response.has_focus()) && !ui.ctx().text_edit_focused();
        let zoom_before_keys = self.camera.target_view().zoom;
        if self.camera.handle_keys(ui, rect, accepts_keys) {
            if (self.camera.target_view().zoom - zoom_before_keys).abs() > f32::EPSILON {
                self.canvas.record_zoom_input();
            }
            ui.ctx().request_repaint();
        }

        let camera_frame = self.camera.frame(rect, study.summary(), stable_dt);
        if camera_frame.animating {
            ui.ctx().request_repaint();
        }
        let workspace_interaction = self.handle_workspace_interaction(
            ui,
            response,
            rect,
            study.summary(),
            camera_frame.rendered,
            accepts_keys,
        );

        self.apply_canvas_gestures(
            ui,
            response,
            rect,
            camera_frame.rendered,
            workspace_interaction,
        );

        // Pointer input can change the target after this frame's rendered view was
        // advanced. Publish that new target immediately so tile lookahead starts during
        // the gesture instead of waiting for the next animation frame.
        let camera_frame = self
            .camera
            .retarget_frame(camera_frame.rendered, study.summary());
        if camera_frame.animating {
            ui.ctx().request_repaint();
        }

        camera_frame
    }

    fn apply_canvas_gestures(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
        rendered: CameraView,
        workspace_interaction: WorkspaceCanvasInteraction,
    ) {
        if workspace_interaction.pan_requested {
            self.camera.pan_by_rendered(response.drag_delta(), rendered);
            ui.ctx().request_repaint();
        }
        if response.double_clicked() && !workspace_interaction.click_consumed {
            let pointer = response.interact_pointer_pos().unwrap_or(rect.center());
            self.canvas.record_zoom_input();
            self.camera
                .zoom_around_rendered(rect, pointer, 2.0, rendered);
            ui.ctx().request_repaint();
        }

        if response.hovered() {
            let scroll_y = ui.input(raw_wheel_delta_y);
            if scroll_y.abs() > 0.0 {
                let pointer = ui
                    .input(|input| input.pointer.hover_pos())
                    .unwrap_or(rect.center());
                self.canvas.record_zoom_input();
                self.camera.zoom_around_rendered(
                    rect,
                    pointer,
                    wheel_zoom_factor(scroll_y, self.wheel_zoom),
                    rendered,
                );
                ui.ctx().request_repaint();
            }
            let pinch = ui.input(|input| input.zoom_delta());
            if (pinch - 1.0).abs() > 0.001 {
                let pointer = ui
                    .input(|input| input.pointer.hover_pos())
                    .unwrap_or(rect.center());
                self.canvas.record_zoom_input();
                self.camera
                    .zoom_around_rendered(rect, pointer, pinch, rendered);
                ui.ctx().request_repaint();
            }
            if ui.input(|input| input.pointer.any_down()) {
                ui.ctx().request_repaint();
            }
        }
    }

    fn paint_annotation_overlays(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        study: &ViewerStudy,
        response: &egui::Response,
        rendered: CameraView,
    ) {
        let hover_base = response
            .hover_pos()
            .map(|p| screen_to_base(rect, p, rendered.center_base, rendered.zoom));
        let tile_failure = self.canvas.tile_failure();
        let debug_stats = self.canvas.debug_stats_text();
        if should_draw_canvas_hud(self.pending_viewport_export.is_some()) {
            draw_canvas_overlays(
                painter,
                rect,
                OverlayInfo {
                    summary: study.summary(),
                    zoom: rendered.zoom,
                    frame_rate: self.frame_stats.info(),
                    hover_base,
                    tile_failure,
                    debug_stats: debug_stats.as_deref(),
                },
            );
        }
        if let Some(runtime) = &self.workspace {
            if let Some(context) = study.annotation_context() {
                draw_external_layer_overlays(painter, rect, runtime, context, rendered);
            }
            draw_workspace_overlay(painter, rect, runtime, rendered);
        }
    }
}
