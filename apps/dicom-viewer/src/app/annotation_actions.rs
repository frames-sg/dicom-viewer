use std::path::PathBuf;

use eframe::egui;

use super::annotation_job::{spawn_annotation_load, LoadedSidecar};
use super::background_worker::{BackgroundWorker, WorkerPoll};
use super::export_job::WorkspaceExportKind;
use super::workspace_actions::{
    confirm_eligible_only_list, ensure_export_not_cancelled, same_file_target,
};
use super::DicomViewerApp;

impl DicomViewerApp {
    pub(super) fn poll_annotation_jobs(&mut self, ctx: &egui::Context) {
        let load_poll = self
            .annotation_load_job
            .as_ref()
            .map(BackgroundWorker::poll);
        if let Some(WorkerPoll::Complete(result)) = load_poll {
            self.annotation_load_job = None;
            match result.result {
                Ok(LoadedSidecar::Annotation(document)) => {
                    let group_count = document.groups().len();
                    let name = result
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Imported ANN")
                        .to_owned();
                    match self.workspace.as_mut().map(|runtime| {
                        runtime.add_external_annotation(
                            name,
                            Some(result.path.clone()),
                            document,
                        )
                    }) {
                        Some(Ok(_)) => {
                            self.status = format!(
                                "Loaded {group_count} ANN group(s) as a read-only source layer from {}.",
                                result.path.display()
                            )
                        }
                        Some(Err(error)) => self.status = error.to_string(),
                        None => self.status = "The pathology workspace is unavailable.".into(),
                    }
                }
                Ok(LoadedSidecar::Segmentation(document)) => {
                    let kind = document.kind();
                    let segment_count = document.segments().len();
                    let name = result
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Imported SEG")
                        .to_owned();
                    match self.workspace.as_mut().map(|runtime| {
                        runtime.add_external_segmentation(name, Some(result.path.clone()), document)
                    }) {
                        Some(Ok((_, diagnostics))) => {
                            let mut loss_codes = diagnostics
                                .iter()
                                .filter(|diagnostic| diagnostic.blocks_roundtrip())
                                .map(|diagnostic| diagnostic.code())
                                .collect::<Vec<_>>();
                            loss_codes.sort_unstable();
                            loss_codes.dedup();
                            self.status = if kind == dicom_viewer_core::SegmentationKind::Fractional
                            {
                                format!(
                                    "Loaded {segment_count} fractional SEG segment(s) as a read-only raster overlay from {}.",
                                    result.path.display()
                                )
                            } else if loss_codes.is_empty() {
                                format!(
                                    "Loaded and vectorized {segment_count} SEG segment(s) from {}.",
                                    result.path.display()
                                )
                            } else {
                                format!(
                                    "Loaded and vectorized {segment_count} SEG segment(s) with explicit projection losses [{}] from {}.",
                                    loss_codes.join(", "),
                                    result.path.display()
                                )
                            };
                        }
                        Some(Err(error)) => self.status = error.to_string(),
                        None => self.status = "The pathology workspace is unavailable.".into(),
                    }
                }
                Err(error) => {
                    self.status = format!("Failed to load {}: {error}", result.path.display());
                }
            }
            ctx.request_repaint();
        } else if matches!(load_poll, Some(WorkerPoll::Disconnected)) {
            self.annotation_load_job = None;
            self.status = "Annotation load worker exited without returning a result.".into();
        }
    }

    pub(super) fn start_annotation_load(
        &mut self,
        path: PathBuf,
        kind: Option<dicom_viewer_core::SidecarKind>,
        ctx: &egui::Context,
    ) {
        let Some(context) = self
            .study
            .as_ref()
            .and_then(|study| study.annotation_context())
            .cloned()
        else {
            self.status = "DICOM sidecars require an open VL WSI DICOM source.".into();
            return;
        };
        if self.annotation_load_job.is_some() {
            self.status = "Another annotation sidecar is already loading.".into();
            return;
        }
        match spawn_annotation_load(path.clone(), kind, context, ctx) {
            Ok(job) => {
                self.annotation_load_job = Some(job);
                self.status = format!("Loading {}…", path.display());
            }
            Err(error) => {
                self.status = format!("Could not start DICOM annotation import: {error}");
            }
        }
    }

    pub(super) fn pick_annotation_sidecar(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().add_filter("DICOM ANN or SEG", &["dcm", "dicom"]);
        if let Some(directory) = self.dicom_source_directory() {
            dialog = dialog.set_directory(directory);
        }
        if let Some(path) = dialog.pick_file() {
            self.start_annotation_load(path, None, ctx);
        }
    }

    pub(super) fn save_dicom_ann(&mut self, ctx: &egui::Context) {
        self.save_dicom_ann_with_mode(ctx, false);
    }

    pub(super) fn save_compatibility_dicom_ann(&mut self, ctx: &egui::Context) {
        self.save_dicom_ann_with_mode(ctx, true);
    }

    fn save_dicom_ann_with_mode(&mut self, ctx: &egui::Context, compatibility: bool) {
        if self.workspace_export_job.is_some() {
            self.status = "Another DICOM annotation export is already running.".into();
            return;
        }
        let Some(context) = self
            .study
            .as_ref()
            .and_then(|study| study.annotation_context())
            .cloned()
        else {
            self.status = "DICOM ANN export requires a VL WSI DICOM source.".into();
            return;
        };
        let Some(document) = self
            .workspace
            .as_ref()
            .map(|runtime| runtime.document_snapshot())
        else {
            self.status = "The pathology workspace is unavailable.".into();
            return;
        };
        let excluded_segments = if compatibility {
            0
        } else {
            document.segments().count()
        };
        let excluded_rulers = document.measurements().len();
        if (excluded_segments > 0 || excluded_rulers > 0)
            && !confirm_eligible_only_list(
                &[
                    (excluded_segments, "segmentation segment"),
                    (excluded_rulers, "ruler measurement"),
                ],
                "SEG or SR",
            )
        {
            self.status =
                "ANN export cancelled; incompatible content was not silently omitted.".into();
            return;
        }
        let (export_kind, stem) = if compatibility {
            (
                WorkspaceExportKind::CompatibilityDicomAnn,
                "tumor-mask-compatibility",
            )
        } else {
            (WorkspaceExportKind::DicomAnn, "annotations")
        };
        let Some(path) = self.choose_export_path(
            "DICOM ANN",
            &["dcm", "dicom"],
            &dicom_default_name(&context, stem, "ann"),
        ) else {
            return;
        };
        if same_file_target(&path, context.source_path()) {
            self.status = "Refusing to overwrite the original WSI DICOM.".into();
            return;
        }
        self.start_workspace_export(path, export_kind, ctx, move |temporary, cancellation| {
            ensure_export_not_cancelled(cancellation)?;
            let ann = if compatibility {
                document.export_tumor_mask_compatibility_ann(&context)
            } else {
                document.export_ann(&context)
            }
            .map_err(|error| error.to_string())?;
            ensure_export_not_cancelled(cancellation)?;
            ann.write_ann(temporary).map_err(|error| error.to_string())
        });
    }

    pub(super) fn export_dicom_seg(&mut self, ctx: &egui::Context) {
        if self.workspace_export_job.is_some() {
            self.status = "Another DICOM annotation export is already running.".into();
            return;
        }
        let Some(context) = self
            .study
            .as_ref()
            .and_then(|study| study.annotation_context())
            .cloned()
        else {
            self.status = "DICOM SEG export requires a VL WSI DICOM source.".into();
            return;
        };
        let policy = if self.seg_rasterize_vectors {
            dicom_viewer_core::VectorSegmentationPolicy::Rasterize
        } else {
            dicom_viewer_core::VectorSegmentationPolicy::Exclude
        };
        let Some(document) = self
            .workspace
            .as_ref()
            .map(|runtime| runtime.document_snapshot())
        else {
            self.status = "The pathology workspace is unavailable.".into();
            return;
        };
        let vector_points = document
            .vector_findings()
            .filter(|finding| {
                matches!(
                    finding.geometry(),
                    dicom_viewer_core::VectorFindingGeometry::Point(_)
                )
            })
            .count();
        let vector_regions = document
            .vector_findings()
            .filter(|finding| {
                matches!(
                    finding.geometry(),
                    dicom_viewer_core::VectorFindingGeometry::Regions(_)
                )
            })
            .count();
        let excluded_regions = if policy == dicom_viewer_core::VectorSegmentationPolicy::Exclude {
            vector_regions
        } else {
            0
        };
        let excluded_rulers = document.measurements().len();
        if (vector_points > 0 || excluded_regions > 0 || excluded_rulers > 0)
            && !confirm_eligible_only_list(
                &[
                    (vector_points, "vector point finding"),
                    (excluded_regions, "vector region finding"),
                    (excluded_rulers, "ruler measurement"),
                ],
                "ANN, SR, or scheme-aware GeoJSON",
            )
        {
            self.status =
                "SEG export cancelled; incompatible content was not silently omitted.".into();
            return;
        }
        let Some(path) = self.choose_export_path(
            "DICOM SEG",
            &["dcm", "dicom"],
            &dicom_default_name(&context, "segmentation", "seg"),
        ) else {
            return;
        };
        if same_file_target(&path, context.source_path()) {
            self.status = "Refusing to overwrite the original WSI DICOM.".into();
            return;
        }
        self.start_workspace_export(
            path,
            WorkspaceExportKind::DicomSeg,
            ctx,
            move |temporary, cancellation| {
                ensure_export_not_cancelled(cancellation)?;
                let segmentation = document
                    .export_seg(&context, policy)
                    .map_err(|error| error.to_string())?;
                ensure_export_not_cancelled(cancellation)?;
                segmentation
                    .write_seg(temporary)
                    .map_err(|error| error.to_string())
            },
        );
    }
}

fn dicom_default_name(
    context: &dicom_viewer_core::DicomAnnotationContext,
    suffix: &str,
    short_kind: &str,
) -> String {
    context
        .source_path()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(
            || format!("wsi_{suffix}.dcm"),
            |stem| format!("{stem}_{short_kind}.dcm"),
        )
}
