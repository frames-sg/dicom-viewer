use eframe::egui;
use std::sync::Arc;

use super::bounded_input::read_bounded;
use super::export_job::{WorkspaceExportJob, WorkspaceExportKind};
use super::raster::{export_raster_cancellable, RasterEvent, DEFAULT_MAX_INSTANCE_BYTES};
use super::workspace::ExternalLayerPayload;
use super::DicomViewerApp;
use dicom_viewer_core::{RasterInputFormat, RasterProfile};

const MAX_PROFILE_BYTES: u64 = 4 * 1024 * 1024;

impl DicomViewerApp {
    pub(super) fn poll_raster_job(&mut self, context: &egui::Context) {
        let Some(event) = self.raster.poll() else {
            return;
        };
        self.status = match event {
            RasterEvent::Loaded(session) => {
                let frame_count = session.frame_count();
                let channel_count = session.selected_channel_count();
                let name = session
                    .raster_path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Raster overlay")
                    .to_owned();
                match self.workspace.as_mut().map(|runtime| {
                    runtime.add_external_heatmap(name, *session, context)
                }) {
                    Some(Ok(_)) => format!(
                        "Loaded profiled raster: {channel_count} parameter(s), {frame_count} DICOM frame(s)."
                    ),
                    Some(Err(error)) => {
                        format!("Loaded raster but could not register its source layer: {error}")
                    }
                    None => "The pathology workspace is unavailable.".into(),
                }
            }
            RasterEvent::Failed(error) => format!("Failed to import profiled raster: {error}"),
            RasterEvent::Disconnected => {
                "Raster conversion worker exited without returning a result.".into()
            }
        };
    }

    pub(super) fn pick_profiled_raster(&mut self, ctx: &egui::Context) {
        let Some(context) = self
            .study
            .as_ref()
            .and_then(|study| study.annotation_context())
            .cloned()
        else {
            self.status = "Raster conversion requires an open VL WSI DICOM source.".into();
            return;
        };
        let mut profile_dialog = rfd::FileDialog::new()
            .add_filter("Raster profile", &["json"])
            .set_file_name("raster-profile-v1.json");
        if let Some(directory) = context.source_path().parent() {
            profile_dialog = profile_dialog.set_directory(directory);
        }
        let Some(profile_path) = profile_dialog.pick_file() else {
            return;
        };
        let profile_bytes = match read_bounded(&profile_path, MAX_PROFILE_BYTES, "raster profile") {
            Ok(bytes) => bytes,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let profile = match RasterProfile::from_json(&profile_bytes) {
            Ok(profile) => profile,
            Err(error) => {
                self.status = format!("Invalid raster profile: {error}");
                return;
            }
        };
        let directory = profile_path.parent();
        let raster_path = match profile.input_format() {
            RasterInputFormat::Zarr => {
                let mut dialog = rfd::FileDialog::new();
                if let Some(directory) = directory {
                    dialog = dialog.set_directory(directory);
                }
                dialog.pick_folder()
            }
            format => {
                let (label, extensions): (&str, &[&str]) = match format {
                    RasterInputFormat::Tiff => ("TIFF raster", &["tif", "tiff"]),
                    RasterInputFormat::Npy => ("NumPy raster", &["npy"]),
                    RasterInputFormat::TiledManifest => ("Tiled manifest", &["json"]),
                    RasterInputFormat::Zarr => unreachable!(),
                };
                let mut dialog = rfd::FileDialog::new().add_filter(label, extensions);
                if let Some(directory) = directory {
                    dialog = dialog.set_directory(directory);
                }
                dialog.pick_file()
            }
        };
        let Some(raster_path) = raster_path else {
            return;
        };
        match self
            .raster
            .start_load(context, profile_path, raster_path.clone(), ctx)
        {
            Ok(()) => self.status = format!("Scanning profiled raster {}…", raster_path.display()),
            Err(error) => self.status = error,
        }
    }

    pub(super) fn pick_pm_bundle_destination(&mut self, ctx: &egui::Context) {
        if self.workspace_export_job.is_some() {
            self.status = "Another export is already running.".into();
            return;
        }
        let Some(session) = self.workspace.as_ref().and_then(|runtime| {
            runtime
                .document()
                .external_layers()
                .iter()
                .rev()
                .find_map(|layer| match runtime.external_payload(layer.id()) {
                    Some(ExternalLayerPayload::Heatmap { session, .. }) => {
                        Some(Arc::clone(session))
                    }
                    _ => None,
                })
        }) else {
            self.status = "Import a profiled heatmap before exporting Parametric Map.".into();
            return;
        };
        let mut dialog = rfd::FileDialog::new().set_file_name("parametric-map-bundle");
        if let Some(directory) = self.dicom_source_directory() {
            dialog = dialog.set_directory(directory);
        }
        let Some(destination) = dialog.save_file() else {
            return;
        };
        match WorkspaceExportJob::spawn_published(
            destination.clone(),
            WorkspaceExportKind::DicomPm,
            ctx,
            move |destination, cancellation| {
                export_raster_cancellable(&session, destination, cancellation).map(|_| ())
            },
        ) {
            Ok(job) => {
                self.workspace_export_job = Some(job);
                self.status = format!(
                    "Planning and writing verified Parametric Map parts to {} ({} byte limit)…",
                    destination.display(),
                    DEFAULT_MAX_INSTANCE_BYTES
                )
            }
            Err(error) => {
                self.status = format!("Could not start DICOM PM export: {error}");
            }
        }
    }
}
