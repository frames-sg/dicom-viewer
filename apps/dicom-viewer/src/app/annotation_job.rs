use std::path::PathBuf;

use dicom_viewer_core::{
    AnnotationDocument, DicomAnnotationContext, SegmentationDocument, SidecarKind,
};
use eframe::egui;

use super::background_worker::BackgroundWorker;

pub(super) enum LoadedSidecar {
    Annotation(AnnotationDocument),
    Segmentation(SegmentationDocument),
}

pub(super) struct AnnotationLoadResult {
    pub(super) path: PathBuf,
    pub(super) result: std::result::Result<LoadedSidecar, String>,
}

pub(super) fn spawn_annotation_load(
    path: PathBuf,
    kind: Option<SidecarKind>,
    context: DicomAnnotationContext,
    repaint: &egui::Context,
) -> std::io::Result<BackgroundWorker<AnnotationLoadResult>> {
    BackgroundWorker::spawn("dicom-viewer-annotation-load", repaint, move || {
        let result = load_sidecar(&path, kind, &context);
        AnnotationLoadResult { path, result }
    })
}

fn load_sidecar(
    path: &PathBuf,
    kind: Option<SidecarKind>,
    context: &DicomAnnotationContext,
) -> std::result::Result<LoadedSidecar, String> {
    match kind {
        Some(SidecarKind::Annotation) => AnnotationDocument::read_ann(path, context)
            .map(LoadedSidecar::Annotation)
            .map_err(|error| error.to_string()),
        Some(
            SidecarKind::BinarySegmentation
            | SidecarKind::FractionalSegmentation
            | SidecarKind::LabelMapSegmentation,
        ) => SegmentationDocument::read_seg(path, context)
            .map(LoadedSidecar::Segmentation)
            .map_err(|error| error.to_string()),
        Some(SidecarKind::StructuredReport) => {
            Err("structured reports must be opened by the SR loader".into())
        }
        None => AnnotationDocument::read_ann(path, context)
            .map(LoadedSidecar::Annotation)
            .or_else(|ann_error| {
                SegmentationDocument::read_seg(path, context)
                    .map(LoadedSidecar::Segmentation)
                    .map_err(|seg_error| {
                        dicom_viewer_core::ViewerError::Unsupported(format!(
                            "not a supported ANN ({ann_error}) or SEG ({seg_error})"
                        ))
                    })
            })
            .map_err(|error| error.to_string()),
    }
}
