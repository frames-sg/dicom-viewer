use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::background_worker::WorkerPoll;
use super::export_job::{WorkspaceExportJob, WorkspaceExportKind};
use super::ui::pathology_workspace::PathologyWorkspaceActions;
use super::DicomViewerApp;
use dicom_viewer_core::{
    frames_viewer_producer, LinearMeasurementSpec, MeasurementReportSemantics,
    StructuredReportDocument,
};
use eframe::egui;

impl DicomViewerApp {
    pub(super) fn handle_pathology_workspace_actions(
        &mut self,
        actions: PathologyWorkspaceActions,
        ctx: &egui::Context,
    ) {
        if let Some(error) = actions.error {
            self.status = error;
        }
        if let Some(status) = actions.status {
            self.status = status;
        }
        if actions.delete_selection {
            match self
                .workspace
                .as_mut()
                .map(|runtime| runtime.delete_selection())
            {
                Some(Ok(count)) => self.status = format!("Deleted {count} tracked object(s)."),
                Some(Err(error)) => self.status = error.to_string(),
                None => {}
            }
        }
        if let Some(id) = actions.jump_to {
            let bounds = self.workspace.as_mut().and_then(|runtime| {
                runtime.refresh_spatial_index().ok()?;
                runtime.select_only(id);
                runtime.object_bounds(id)
            });
            if let Some(bounds) = bounds {
                self.camera.center_on_base_bounds(bounds);
                self.status = "Centered the selected finding.".into();
            }
        }

        if actions.import_dicom {
            self.pick_annotation_sidecar(ctx);
        }
        if actions.import_profiled_geojson {
            self.pick_profiled_geojson(ctx);
        }
        if actions.import_sr {
            self.pick_structured_report(ctx, false);
        }
        if actions.import_sr_with_seg {
            self.pick_structured_report(ctx, true);
        }
        if actions.import_raster {
            self.pick_profiled_raster(ctx);
        }
        if let Some((path, kind)) = actions.load_sidecar {
            if kind == dicom_viewer_core::SidecarKind::StructuredReport {
                self.start_structured_report_path(path, None, ctx);
            } else {
                self.start_annotation_load(path, Some(kind), ctx);
            }
        }

        if actions.export_portable_workspace {
            self.export_portable_workspace(ctx);
        }
        if actions.export_scheme_geojson {
            self.export_scheme_geojson(ctx);
        }
        if actions.export_compatibility_geojson {
            self.export_compatibility_geojson(ctx);
        }
        if actions.export_ann {
            self.save_dicom_ann(ctx);
        }
        if actions.export_compatibility_ann {
            self.save_compatibility_dicom_ann(ctx);
        }
        if actions.export_seg {
            self.export_dicom_seg(ctx);
        }
        if actions.export_sr {
            self.export_measurement_sr(ctx);
        }
        if actions.export_pm {
            self.pick_pm_bundle_destination(ctx);
        }
        if actions.install_scheme {
            self.show_scheme_settings = true;
        }
        if actions.workspace_storage {
            self.show_storage_settings = true;
        }
    }

    pub(super) fn poll_workspace_export_job(&mut self, ctx: &egui::Context) {
        let poll = self
            .workspace_export_job
            .as_ref()
            .map(WorkspaceExportJob::poll);
        match poll {
            Some(WorkerPoll::Complete(result)) => {
                self.workspace_export_job = None;
                self.status = match result.result {
                    Ok(()) => format!(
                        "Saved {} to {}.",
                        result.kind.label(),
                        result.destination.display()
                    ),
                    Err(error) => format!(
                        "Failed to save {} to {}: {error}",
                        result.kind.label(),
                        result.destination.display()
                    ),
                };
                ctx.request_repaint();
            }
            Some(WorkerPoll::Disconnected) => {
                self.workspace_export_job = None;
                self.status = "Export worker exited without returning a result; the destination was not changed.".into();
            }
            Some(WorkerPoll::Pending) | None => {}
        }
    }

    pub(in crate::app) fn start_workspace_export(
        &mut self,
        destination: PathBuf,
        kind: WorkspaceExportKind,
        ctx: &egui::Context,
        write: impl FnOnce(&Path, &AtomicBool) -> Result<(), String> + Send + 'static,
    ) {
        if self.workspace_export_job.is_some() {
            self.status = "Another export is already running.".into();
            return;
        }
        match WorkspaceExportJob::spawn(destination.clone(), kind, ctx, write) {
            Ok(job) => {
                self.workspace_export_job = Some(job);
                self.status = format!("Saving {} to {}…", kind.label(), destination.display());
            }
            Err(error) => {
                self.status = format!("Could not start {} export: {error}", kind.label());
            }
        }
    }

    pub(super) fn export_portable_workspace(&mut self, ctx: &egui::Context) {
        let Some(runtime) = &self.workspace else {
            return;
        };
        let bytes = match runtime.document().to_json() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.status = format!("Portable workspace preflight failed: {error}");
                return;
            }
        };
        let Some(path) = self.choose_export_path(
            "Portable pathology workspace",
            &["json"],
            "pathology-workspace.json",
        ) else {
            return;
        };
        self.start_workspace_export(
            path,
            WorkspaceExportKind::PortableWorkspace,
            ctx,
            move |temporary, cancellation| write_export_bytes(temporary, &bytes, cancellation),
        );
    }

    pub(super) fn export_scheme_geojson(&mut self, ctx: &egui::Context) {
        let Some(runtime) = &self.workspace else {
            return;
        };
        let export = match runtime.document().export_pathology_geojson() {
            Ok(export) => export,
            Err(error) => {
                self.status = format!("Pathology GeoJSON preflight failed: {error}");
                return;
            }
        };
        let excluded = export.excluded_measurement_count();
        if excluded > 0 && !confirm_eligible_only_list(&[(excluded, "ruler measurement")], "SR") {
            self.status = "GeoJSON export cancelled; rulers were not silently omitted.".into();
            return;
        }
        let Some(path) =
            self.choose_export_path("Pathology GeoJSON", &["geojson"], "pathology.geojson")
        else {
            return;
        };
        let bytes = export.bytes().to_vec();
        self.start_workspace_export(
            path,
            WorkspaceExportKind::SchemeGeoJson,
            ctx,
            move |temporary, cancellation| write_export_bytes(temporary, &bytes, cancellation),
        );
    }

    pub(super) fn export_compatibility_geojson(&mut self, ctx: &egui::Context) {
        let Some(runtime) = &self.workspace else {
            return;
        };
        let excluded_findings = runtime
            .document()
            .vector_findings()
            .filter(|finding| {
                finding.class_id() != "viable-tumor"
                    || !matches!(
                        finding.geometry(),
                        dicom_viewer_core::VectorFindingGeometry::Regions(_)
                    )
            })
            .count();
        let excluded_rulers = runtime.document().measurements().len();
        if (excluded_findings > 0 || excluded_rulers > 0)
            && !confirm_eligible_only_list(
                &[
                    (excluded_findings, "non-tumor vector finding"),
                    (excluded_rulers, "ruler measurement"),
                ],
                "scheme-aware GeoJSON or SR",
            )
        {
            self.status =
                "CellViT compatibility export cancelled; content was not silently omitted.".into();
            return;
        }
        let bytes = match runtime.document().export_cellvit_compatibility_geojson() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.status = format!("CellViT compatibility preflight failed: {error}");
                return;
            }
        };
        let Some(path) = self.choose_export_path(
            "CellViT viable-tumor GeoJSON",
            &["geojson"],
            "viable_tumor.geojson",
        ) else {
            return;
        };
        self.start_workspace_export(
            path,
            WorkspaceExportKind::CompatibilityGeoJson,
            ctx,
            move |temporary, cancellation| write_export_bytes(temporary, &bytes, cancellation),
        );
    }

    pub(super) fn export_measurement_sr(&mut self, ctx: &egui::Context) {
        let Some(context) = self
            .study
            .as_ref()
            .and_then(|study| study.annotation_context())
            .cloned()
        else {
            self.status = "Measurement SR export requires a VL WSI DICOM source.".into();
            return;
        };
        let Some(runtime) = &self.workspace else {
            return;
        };
        let excluded_vectors = runtime.document().vector_findings().count();
        let excluded_segments = runtime.document().segments().count();
        if (excluded_vectors > 0 || excluded_segments > 0)
            && !confirm_eligible_only_list(
                &[
                    (excluded_vectors, "vector finding"),
                    (excluded_segments, "segmentation segment"),
                ],
                "ANN, SEG, or GeoJSON",
            )
        {
            self.status =
                "SR export cancelled; non-measurement content was not silently omitted.".into();
            return;
        }
        let mut specs = Vec::with_capacity(runtime.document().measurements().len());
        for measurement in runtime.document().measurements() {
            let Some(class) = runtime.document().scheme().class(measurement.class_id()) else {
                self.status = "Measurement references an unknown scheme class.".into();
                return;
            };
            let endpoints = measurement.endpoints();
            let mut spec = match LinearMeasurementSpec::new(
                measurement.tracking().clone(),
                class.category().clone(),
                class.property_type().clone(),
                endpoints[0],
                endpoints[1],
            ) {
                Ok(spec) => spec,
                Err(error) => {
                    self.status = format!("Measurement SR preflight failed: {error}");
                    return;
                }
            };
            if let Some(site) = measurement.finding_site() {
                let Some(code) = runtime
                    .document()
                    .scheme()
                    .finding_sites()
                    .iter()
                    .find(|code| site.matches(code))
                    .cloned()
                else {
                    self.status =
                        "Measurement finding site is not controlled by the pinned scheme.".into();
                    return;
                };
                spec = spec.with_finding_sites(vec![code]);
            }
            specs.push(spec);
        }
        let Some(path) = self.choose_export_path(
            "DICOM Structured Report",
            &["dcm", "dicom"],
            "pathology-measurements-sr.dcm",
        ) else {
            return;
        };
        self.start_workspace_export(
            path,
            WorkspaceExportKind::DicomSr,
            ctx,
            move |temporary, cancellation| {
                ensure_export_not_cancelled(cancellation)?;
                let document = StructuredReportDocument::from_linear_measurements(
                    context,
                    &MeasurementReportSemantics::pathology_v1(),
                    &specs,
                )
                .map_err(|error| error.to_string())?
                .with_producer(
                    frames_viewer_producer(9301, "WSI measurement reports")
                        .map_err(|error| error.to_string())?,
                );
                ensure_export_not_cancelled(cancellation)?;
                document
                    .write_sr(temporary)
                    .map_err(|error| error.to_string())
            },
        );
    }

    pub(in crate::app) fn choose_export_path(
        &mut self,
        label: &str,
        extensions: &[&str],
        default_name: &str,
    ) -> Option<PathBuf> {
        loop {
            let mut dialog = rfd::FileDialog::new()
                .add_filter(label, extensions)
                .set_file_name(default_name);
            if let Some(parent) = self.active_path.as_deref().and_then(Path::parent) {
                dialog = dialog.set_directory(parent);
            }
            let path = dialog.save_file()?;
            if self
                .active_path
                .as_deref()
                .is_some_and(|source| same_file_target(&path, source))
            {
                self.status = "Refusing to overwrite the opened source.".into();
                continue;
            }
            if !path.exists() {
                return Some(path);
            }
            let decision = rfd::MessageDialog::new()
                .set_title("Destination already exists")
                .set_description(format!(
                    "{} already exists. Replace it, choose another destination, or cancel?",
                    path.display()
                ))
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
                    "Replace".into(),
                    "Choose Another".into(),
                    "Cancel".into(),
                ))
                .show();
            match decision {
                rfd::MessageDialogResult::Custom(label) if label == "Replace" => {
                    return Some(path);
                }
                rfd::MessageDialogResult::Custom(label) if label == "Choose Another" => continue,
                _ => return None,
            }
        }
    }
}

pub(in crate::app) fn ensure_export_not_cancelled(cancellation: &AtomicBool) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        Err("Export cancelled; the destination was not changed.".into())
    } else {
        Ok(())
    }
}

fn write_export_bytes(
    temporary: &Path,
    bytes: &[u8],
    cancellation: &AtomicBool,
) -> Result<(), String> {
    ensure_export_not_cancelled(cancellation)?;
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(temporary)
        .map_err(|error| format!("Could not open export temporary file: {error}"))?;
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Could not write export temporary file: {error}"))?;
    ensure_export_not_cancelled(cancellation)
}

pub(in crate::app) fn confirm_eligible_only_list(items: &[(usize, &str)], target: &str) -> bool {
    let summary = eligible_exclusion_summary(items);
    rfd::MessageDialog::new()
        .set_title("Some selected content is not eligible")
        .set_description(format!(
            "{summary} cannot be represented by this format and belong in {target}. Export eligible items only?"
        ))
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

fn eligible_exclusion_summary(items: &[(usize, &str)]) -> String {
    items
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, kind)| format!("{count} {kind}{}", if *count == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(in crate::app) fn same_file_target(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusion_summary_lists_only_nonzero_ineligible_content() {
        assert_eq!(
            eligible_exclusion_summary(&[
                (2, "segmentation segment"),
                (0, "vector point"),
                (1, "ruler measurement"),
            ]),
            "2 segmentation segments, 1 ruler measurement"
        );
    }
}
