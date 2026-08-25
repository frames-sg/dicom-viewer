use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dicom_viewer_core::{TileDecodeBackend, ViewerStudy};
use eframe::egui::{self, Frame, Sense};

mod annotation_actions;
mod annotation_job;
mod background_worker;
mod bounded_input;
mod camera;
mod canvas;
mod export_job;
mod format;
mod level_warmer;
mod open_job;
mod pathology;
mod pathology_actions;
mod raster;
mod raster_actions;
mod report;
mod report_actions;
mod theme;
mod tile;
mod ui;
mod viewport;
mod workspace;
mod workspace_actions;
mod workspace_dialogs;
mod workspace_interaction;

use annotation_job::AnnotationLoadResult;
use background_worker::BackgroundWorker;
use camera::{wheel_zoom_factor, CameraState};
#[cfg(test)]
use camera::{CameraMotion, CameraView, MAX_ZOOM, MIN_ZOOM};
use canvas::SlideCanvas;
use export_job::WorkspaceExportJob;
use open_job::{OpenPoll, OpenQueue};
use pathology::PathologyState;
use raster::RasterState;
use report::ReportState;
use ui::chrome::{show_status_bar, show_toolbar, ToolbarState};
use ui::facts::show_facts_sidebar;
use ui::overlay::{
    draw_canvas_overlays, paint_canvas_background, paint_empty_state, FrameStats, OverlayInfo,
};
use ui::pathology_workspace::{show_pathology_workspace_panel, show_tool_rail};
use viewport::screen_to_base;
#[cfg(test)]
use viewport::{choose_render_level, visible_tiles};
use workspace::{
    draw_external_layer_overlays, draw_workspace_overlay, AutosaveStatus, RestoredWorkspace,
    RevisionStore, SchemeLibrary, WorkspaceAutosave, WorkspaceRuntime, WorkspaceSaveRequest,
};

#[cfg(test)]
use dicom_viewer_core::{LevelIndex, LevelInfo, SourceKind, StudySummary};
#[cfg(test)]
use eframe::egui::{pos2, vec2, Rect};
#[cfg(test)]
use ui::overlay::{fps_color, FrameRateInfo};

const MAX_LOADER_MESSAGES_PER_FRAME: usize = 128;
const LOADER_MESSAGE_BUDGET: Duration = Duration::from_millis(2);
const MAX_VISIBLE_UPLOADS_PER_FRAME: usize = 24;
const VISIBLE_UPLOAD_BUDGET: Duration = Duration::from_millis(6);
const MAX_TRANSITION_UPLOADS_PER_FRAME: usize = 2;
const MAX_PREFETCH_UPLOADS_PER_FRAME: usize = 4;
const PREFETCH_UPLOAD_BUDGET: Duration = Duration::from_millis(1);

pub struct DicomViewerApp {
    study: Option<Arc<ViewerStudy>>,
    open_queue: OpenQueue,
    active_generation: u64,
    next_generation: u64,
    status: String,
    canvas: SlideCanvas,
    frame_stats: FrameStats,
    camera: CameraState,
    show_facts_panel: bool,
    pathology: PathologyState,
    report: ReportState,
    raster: RasterState,
    annotation_load_job: Option<BackgroundWorker<AnnotationLoadResult>>,
    workspace_export_job: Option<WorkspaceExportJob>,
    active_path: Option<PathBuf>,
    reported_cpu_fallbacks: usize,
    workspace: Option<WorkspaceRuntime>,
    revision_store: Option<RevisionStore>,
    autosave: Option<WorkspaceAutosave>,
    pending_restore: Option<RestoredWorkspace>,
    scheme_library: SchemeLibrary,
    show_import_wizard: bool,
    show_export_wizard: bool,
    show_scheme_settings: bool,
    show_storage_settings: bool,
    pending_scheme_migration: Option<workspace_dialogs::PendingSchemeMigration>,
    seg_rasterize_vectors: bool,
    last_queued_workspace_revision: Option<u64>,
    last_queued_draft: Option<workspace::DraftInteraction>,
}

impl DicomViewerApp {
    fn dicom_source_directory(&self) -> Option<&Path> {
        self.study
            .as_ref()
            .and_then(|study| study.annotation_context())
            .and_then(|context| context.source_path().parent())
    }

    fn initialize_workspace(&mut self) {
        let Some(study) = &self.study else {
            return;
        };
        let source_identity = study.source_identity();
        let discovered_sidecars = study.sidecars().to_vec();
        let mut runtime = match WorkspaceRuntime::new(
            source_identity.clone(),
            dicom_viewer_core::AnnotationScheme::general_pathology_v1(),
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                self.status = format!("Could not initialize pathology workspace: {error}");
                return;
            }
        };
        if let Err(error) = reconcile_discovered_sidecar_stubs(&mut runtime, &discovered_sidecars) {
            self.status = format!("Could not register discovered pathology sidecars: {error}");
        }
        self.workspace = Some(runtime);
        self.pending_restore = self
            .revision_store
            .as_ref()
            .and_then(|store| store.restore_latest(&source_identity).ok().flatten());
        self.autosave = self
            .revision_store
            .clone()
            .and_then(|store| WorkspaceAutosave::new(store, &source_identity).ok());
        self.last_queued_workspace_revision = Some(0);
        self.last_queued_draft = None;
        if let Some(restored) = &self.pending_restore {
            self.status = format!(
                "Saved pathology workspace found: {} object(s). Restore is recommended.",
                restored.document().object_count()
            );
        }
    }

    fn restore_saved_workspace(&mut self) {
        let Some(restored) = self.pending_restore.take() else {
            return;
        };
        match WorkspaceRuntime::from_document(restored.document().clone()) {
            Ok(mut runtime) => {
                let discovered_sidecars = self
                    .study
                    .as_ref()
                    .map(|study| study.sidecars().to_vec())
                    .unwrap_or_default();
                if let Err(error) =
                    reconcile_discovered_sidecar_stubs(&mut runtime, &discovered_sidecars)
                {
                    self.status =
                        format!("Restored the workspace but could not register sidecars: {error}");
                    self.pending_restore = Some(restored);
                    return;
                }
                if let Some(draft) = restored.draft().cloned() {
                    runtime.set_draft(draft);
                }
                self.last_queued_workspace_revision = Some(runtime.document().revision());
                self.last_queued_draft = runtime.draft().cloned();
                self.workspace = Some(runtime);
                self.status = format!(
                    "Restored pathology workspace revision {} with {} object(s).",
                    restored.revision(),
                    restored.document().object_count()
                );
            }
            Err(error) => {
                self.status = format!("Could not restore saved pathology workspace: {error}")
            }
        }
    }

    fn start_fresh_workspace(&mut self) {
        let Some(restored) = self.pending_restore.take() else {
            return;
        };
        if let Some(store) = &self.revision_store {
            match store.archive_current(restored.document().source_identity()) {
                Ok(_) => {
                    self.status =
                        "Started fresh; previous workspace revisions are archived for 30 days."
                            .into();
                }
                Err(error) => {
                    self.status = format!("Could not archive saved workspace: {error}");
                    self.pending_restore = Some(restored);
                }
            }
        }
    }

    fn queue_workspace_autosave(&mut self) {
        let Some(runtime) = &self.workspace else {
            return;
        };
        if runtime.handle_drag_active() || runtime.brush_stroke().is_some() {
            return;
        }
        let revision = runtime.document().revision();
        let draft = runtime.draft().cloned();
        if self.last_queued_workspace_revision == Some(revision) && self.last_queued_draft == draft
        {
            return;
        }
        if let Some(autosave) = &mut self.autosave {
            autosave.queue(
                WorkspaceSaveRequest::new(runtime.document_snapshot(), draft.clone()),
                Instant::now(),
            );
            self.last_queued_workspace_revision = Some(revision);
            self.last_queued_draft = draft;
        }
    }

    fn poll_workspace_autosave(&mut self) {
        if let Some(autosave) = &mut self.autosave {
            autosave.poll(Instant::now());
            if let AutosaveStatus::Failed(error) = autosave.status() {
                self.status = format!("Workspace autosave failed: {error}");
            }
        }
    }

    fn autosave_label(&self) -> String {
        match self.autosave.as_ref().map(WorkspaceAutosave::status) {
            Some(AutosaveStatus::Clean) => "Autosave ready".into(),
            Some(AutosaveStatus::Pending) => "Unsaved changes".into(),
            Some(AutosaveStatus::Saving) => "Saving…".into(),
            Some(AutosaveStatus::Saved { .. }) => "Saved".into(),
            Some(AutosaveStatus::Failed(_)) => "Save failed".into(),
            None => "Autosave unavailable".into(),
        }
    }

    pub fn new(cc: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        theme::install_visuals(&cc.egui_ctx);
        let render_state = cc
            .wgpu_render_state
            .clone()
            .expect("DICOM viewer requires the configured wgpu renderer");
        let canvas = SlideCanvas::new(render_state);
        let mut initial_status = canvas.backend_warning().map_or_else(
            || "Open a WSI file or DICOM folder.".to_string(),
            |warning| format!("Metal interop unavailable; using CPU → wgpu: {warning}"),
        );
        let revision_store = RevisionStore::application_default().ok();
        let scheme_library =
            revision_store
                .as_ref()
                .map_or_else(SchemeLibrary::with_builtins, |store| {
                    match SchemeLibrary::load_or_builtins(store.root().to_path_buf()) {
                        Ok(library) => library,
                        Err(error) => {
                            initial_status = format!(
                        "{initial_status} Annotation scheme library could not be loaded: {error}"
                    );
                            SchemeLibrary::with_builtins()
                        }
                    }
                });
        let mut app = Self {
            study: None,
            open_queue: OpenQueue::default(),
            active_generation: 0,
            next_generation: 1,
            status: initial_status,
            canvas,
            frame_stats: FrameStats::default(),
            camera: CameraState::default(),
            show_facts_panel: false,
            pathology: PathologyState::default(),
            report: ReportState::default(),
            raster: RasterState::default(),
            annotation_load_job: None,
            workspace_export_job: None,
            active_path: None,
            reported_cpu_fallbacks: 0,
            workspace: None,
            revision_store,
            autosave: None,
            pending_restore: None,
            scheme_library,
            show_import_wizard: false,
            show_export_wizard: false,
            show_scheme_settings: false,
            show_storage_settings: false,
            pending_scheme_migration: None,
            seg_rasterize_vectors: false,
            last_queued_workspace_revision: None,
            last_queued_draft: None,
        };
        if let Some(path) = initial_path {
            app.start_open_path(path, &cc.egui_ctx);
        }
        app
    }

    fn start_open_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self
            .workspace
            .as_ref()
            .is_some_and(|runtime| runtime.draft().is_some())
        {
            let decision = rfd::MessageDialog::new()
                .set_title("Unfinished polygon")
                .set_description(
                    "Finish or discard the unfinished polygon before opening another slide. Resume keeps this slide open.",
                )
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
                    "Finish".into(),
                    "Discard".into(),
                    "Resume".into(),
                ))
                .show();
            match decision {
                rfd::MessageDialogResult::Custom(label) if label == "Finish" => {
                    if let Some(runtime) = &mut self.workspace {
                        if let Err(error) = runtime.finish_draft() {
                            self.status = error.to_string();
                            return;
                        }
                    }
                }
                rfd::MessageDialogResult::Custom(label) if label == "Discard" => {
                    if let Some(runtime) = &mut self.workspace {
                        runtime.discard_draft();
                    }
                }
                _ => {
                    self.status = "Open cancelled; polygon draft resumed.".into();
                    return;
                }
            }
        }
        self.queue_workspace_autosave();
        if let Some(autosave) = &mut self.autosave {
            if let Err(error) = autosave.flush() {
                self.status =
                    format!("Open cancelled because the workspace could not be saved: {error}");
                return;
            }
        }
        let options = match self.canvas.viewer_open_options() {
            Ok(options) => options,
            Err(error) => {
                self.status = format!("Failed to open {}: {error}", path.display());
                return;
            }
        };
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        self.active_generation = generation;
        self.study = None;
        self.canvas.clear();
        self.camera.clear_for_open();
        self.pathology.clear();
        self.report.clear();
        self.raster.clear();
        self.annotation_load_job = None;
        if let Some(job) = &self.workspace_export_job {
            job.cancel();
        }
        self.workspace_export_job = None;
        self.active_path = None;
        self.reported_cpu_fallbacks = 0;
        self.workspace = None;
        self.autosave = None;
        self.pending_restore = None;
        self.pending_scheme_migration = None;
        self.show_import_wizard = false;
        self.show_export_wizard = false;
        self.last_queued_workspace_revision = None;
        self.last_queued_draft = None;
        self.status = format!("Opening {}...", path.display());

        match self
            .open_queue
            .submit(path.clone(), generation, ctx, options)
        {
            Ok(()) => {}
            Err(err) => {
                self.status = format!(
                    "Failed to open {}: could not start open worker: {err}",
                    path.display()
                );
            }
        }
        ctx.request_repaint();
    }

    fn poll_open_job(&mut self, ctx: &egui::Context) {
        match self.open_queue.poll(ctx) {
            Ok(Some(OpenPoll::Result(result))) => {
                let current_generation = self.active_generation;
                if result.generation != current_generation {
                    return;
                }
                match result.result {
                    Ok(study) => {
                        let tile_decode_backend = study.summary().tile_decode_backend;
                        self.camera.reset_for_study(study.summary());
                        self.study = Some(Arc::new(study));
                        self.initialize_workspace();
                        self.active_path = Some(result.path.clone());
                        self.canvas.clear();
                        let path_label = match tile_decode_backend {
                            TileDecodeBackend::Cpu => "CPU → wgpu",
                            TileDecodeBackend::Metal => "Metal preferred → wgpu",
                            TileDecodeBackend::Cuda => "CUDA preferred → wgpu",
                        };
                        self.status = format!("Opened {}. {path_label}.", result.path.display());
                    }
                    Err(err) => {
                        self.active_path = None;
                        self.status = format!("Failed to open {}: {err}", result.path.display());
                    }
                }
                ctx.request_repaint();
            }
            Ok(Some(OpenPoll::Disconnected(path))) => {
                self.status = format!("Failed to open {}: open worker exited", path.display());
            }
            Ok(None) => {}
            Err(error) => {
                self.status = format!("Failed to start pending open worker: {error}");
            }
        }
    }

    fn pick_file(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "WSI",
                &[
                    "dcm", "dicom", "svs", "tif", "tiff", "ndpi", "scn", "bif", "czi", "zvi",
                    "mrxs", "vms", "vmu", "vsi", "svcache", "j2k", "j2c",
                ],
            )
            .add_filter("DICOM", &["dcm", "dicom"])
            .add_filter("TIFF WSI", &["svs", "tif", "tiff", "ndpi", "scn", "bif"])
            .add_filter("JPEG 2000", &["j2k", "j2c"])
            .pick_file()
        {
            self.start_open_path(path, ctx);
        }
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.start_open_path(path, ctx);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input_mut(|input| std::mem::take(&mut input.raw.dropped_files));
        for file in dropped {
            if let Some(path) = file.path {
                self.start_open_path(path, ctx);
                break;
            }
        }
    }
}

fn reconcile_discovered_sidecar_stubs(
    runtime: &mut WorkspaceRuntime,
    sidecars: &[dicom_viewer_core::SidecarMetadata],
) -> dicom_viewer_core::Result<()> {
    for sidecar in sidecars {
        let kind = match sidecar.kind() {
            dicom_viewer_core::SidecarKind::Annotation => {
                dicom_viewer_core::ExternalLayerKind::DicomAnn
            }
            dicom_viewer_core::SidecarKind::BinarySegmentation
            | dicom_viewer_core::SidecarKind::FractionalSegmentation
            | dicom_viewer_core::SidecarKind::LabelMapSegmentation => {
                dicom_viewer_core::ExternalLayerKind::DicomSeg
            }
            dicom_viewer_core::SidecarKind::StructuredReport => {
                dicom_viewer_core::ExternalLayerKind::DicomSr
            }
        };
        let name = sidecar
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("DICOM sidecar")
            .to_owned();
        runtime.ensure_discovered_external_stub(name, kind, sidecar.path().to_path_buf())?;
    }
    Ok(())
}

impl eframe::App for DicomViewerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_workspace_autosave();
        if ctx.input(|input| input.viewport().close_requested()) {
            self.queue_workspace_autosave();
            let save_result = self.autosave.as_mut().map(WorkspaceAutosave::flush);
            if let Some(Err(error)) = save_result {
                let decision = rfd::MessageDialog::new()
                    .set_title("Workspace could not be saved")
                    .set_description(format!(
                        "{error}\n\nRetry saving, quit without saving, or cancel close."
                    ))
                    .set_level(rfd::MessageLevel::Error)
                    .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
                        "Retry".into(),
                        "Quit Without Saving".into(),
                        "Cancel".into(),
                    ))
                    .show();
                match decision {
                    rfd::MessageDialogResult::Custom(label) if label == "Quit Without Saving" => {}
                    rfd::MessageDialogResult::Custom(label) if label == "Retry" => {
                        let retry = self.autosave.as_mut().map(WorkspaceAutosave::flush);
                        if retry.is_some_and(|result| result.is_err()) {
                            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                            self.status = "Close cancelled; workspace save still failed.".into();
                        }
                    }
                    _ => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                        self.status = "Close cancelled; workspace kept open.".into();
                    }
                }
            }
        }
        self.handle_dropped_files(ctx);
        self.poll_open_job(ctx);
        self.poll_annotation_jobs(ctx);
        self.poll_workspace_export_job(ctx);
        self.poll_pathology_job();
        self.poll_report_job();
        self.poll_raster_job(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let app_ui_started = self.canvas.debug_stats_enabled().then(Instant::now);
        let (stable_dt, predicted_dt) = ui.input(|input| (input.stable_dt, input.predicted_dt));
        self.frame_stats.record(stable_dt, predicted_dt);

        let has_study = self.study.is_some();

        let autosave_label = self.autosave_label();
        let (can_undo, can_redo) = self.workspace.as_ref().map_or((false, false), |runtime| {
            (runtime.can_undo(), runtime.can_redo())
        });
        let actions = show_toolbar(
            ui,
            ToolbarState {
                has_study,
                show_facts: &mut self.show_facts_panel,
                can_undo,
                can_redo,
                autosave_status: &autosave_label,
                export_running: self.workspace_export_job.is_some(),
                export_cancel_requested: self
                    .workspace_export_job
                    .as_ref()
                    .is_some_and(WorkspaceExportJob::cancellation_requested),
                smooth_camera: self.camera.smoothing_enabled_mut(),
            },
        );
        if actions.open_file {
            self.pick_file(ui.ctx());
        }
        if actions.open_folder {
            self.pick_folder(ui.ctx());
        }
        // Opening replaces the active study and generation. Refresh the frame
        // snapshot after the modal picker returns so this frame cannot submit
        // work for the previous study under the replacement generation.
        let study = self.study.clone();
        let opening = self.open_queue.is_opening();
        if actions.undo && self.workspace.as_mut().is_some_and(WorkspaceRuntime::undo) {
            self.status = "Undid the last pathology command.".into();
        }
        if actions.redo && self.workspace.as_mut().is_some_and(WorkspaceRuntime::redo) {
            self.status = "Redid the pathology command.".into();
        }
        if actions.import {
            self.show_import_wizard = true;
        }
        if actions.export {
            self.show_export_wizard = true;
        }
        if actions.cancel_export {
            if let Some(job) = &self.workspace_export_job {
                job.cancel();
            }
            self.status = "Cancelling export; the destination will remain unchanged…".into();
        }
        show_status_bar(
            ui,
            &self.status,
            opening,
            study.as_ref().map(|study| study.summary()),
        );
        show_facts_sidebar(
            ui,
            self.show_facts_panel,
            self.active_generation,
            study.as_ref().map(|study| study.summary()),
        );
        if let Some(runtime) = &mut self.workspace {
            if let Some(error) = show_tool_rail(ui, runtime) {
                self.status = error;
            }
            let panel_actions = show_pathology_workspace_panel(ui, runtime);
            self.handle_pathology_workspace_actions(panel_actions, ui.ctx());
        }
        self.show_workspace_dialogs(ui.ctx());
        // ── Central canvas ─────────────────────────────────────────
        egui::CentralPanel::default_margins()
            .frame(Frame::NONE.fill(theme::CANVAS))
            .show_inside(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                let response = ui.allocate_rect(rect, Sense::click_and_drag());
                if response.clicked() || response.drag_started() {
                    response.request_focus();
                } else if ui.input(|input| input.pointer.any_pressed()) && !response.hovered() {
                    response.surrender_focus();
                }
                let painter = ui.painter_at(rect);
                paint_canvas_background(&painter, rect);

                let Some(study) = study.clone() else {
                    paint_empty_state(&painter, rect, opening);
                    return;
                };

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
                    &response,
                    rect,
                    study.summary(),
                    camera_frame.rendered,
                    accepts_keys,
                );

                if workspace_interaction.pan_requested {
                    self.camera
                        .pan_by_rendered(response.drag_delta(), camera_frame.rendered);
                    ui.ctx().request_repaint();
                }
                if response.double_clicked() && !workspace_interaction.click_consumed {
                    let pointer = response.interact_pointer_pos().unwrap_or(rect.center());
                    self.canvas.record_zoom_input();
                    self.camera
                        .zoom_around_rendered(rect, pointer, 2.0, camera_frame.rendered);
                    ui.ctx().request_repaint();
                }

                if response.hovered() {
                    let scroll_y = ui.input(|input| input.smooth_scroll_delta.y);
                    if scroll_y.abs() > 0.0 {
                        let pointer = ui
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(rect.center());
                        self.canvas.record_zoom_input();
                        self.camera.zoom_around_rendered(
                            rect,
                            pointer,
                            wheel_zoom_factor(scroll_y),
                            camera_frame.rendered,
                        );
                        ui.ctx().request_repaint();
                    }
                    let pinch = ui.input(|input| input.zoom_delta());
                    if (pinch - 1.0).abs() > 0.001 {
                        let pointer = ui
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(rect.center());
                        self.canvas.record_zoom_input();
                        self.camera.zoom_around_rendered(
                            rect,
                            pointer,
                            pinch,
                            camera_frame.rendered,
                        );
                        ui.ctx().request_repaint();
                    }
                    if ui.input(|input| input.pointer.any_down()) {
                        ui.ctx().request_repaint();
                    }
                }

                self.canvas.paint(
                    ui.ctx(),
                    &painter,
                    rect,
                    &study,
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

                let hover_base = response.hover_pos().map(|p| {
                    screen_to_base(
                        rect,
                        p,
                        camera_frame.rendered.center_base,
                        camera_frame.rendered.zoom,
                    )
                });
                let tile_failure = self.canvas.tile_failure();
                let debug_stats = self.canvas.debug_stats_text();
                draw_canvas_overlays(
                    &painter,
                    rect,
                    OverlayInfo {
                        summary: study.summary(),
                        zoom: camera_frame.rendered.zoom,
                        frame_rate: self.frame_stats.info(),
                        hover_base,
                        tile_failure,
                        debug_stats: debug_stats.as_deref(),
                    },
                );
                if let Some(runtime) = &self.workspace {
                    if let Some(context) = study.annotation_context() {
                        draw_external_layer_overlays(
                            &painter,
                            rect,
                            runtime,
                            context,
                            camera_frame.rendered,
                        );
                    }
                    draw_workspace_overlay(&painter, rect, runtime, camera_frame.rendered);
                }
            });
        self.queue_workspace_autosave();
        if self
            .autosave
            .as_ref()
            .is_some_and(WorkspaceAutosave::has_pending_write)
        {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if let Some(started) = app_ui_started {
            self.canvas.record_app_ui_cpu_time(started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests;
