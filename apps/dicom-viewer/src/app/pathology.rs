use std::path::{Path, PathBuf};

use super::background_worker::{BackgroundWorker, WorkerPoll};
use dicom_viewer_core::{AnnotationDocument, DicomAnnotationContext, PathologyPreview};
use eframe::egui;

mod io;
#[cfg(test)]
mod tests;

use io::load_pathology;

#[derive(Debug)]
struct PathologyInput {
    source: DicomAnnotationContext,
    geojson_path: PathBuf,
    mapping_path: PathBuf,
}

#[derive(Debug)]
pub(super) struct PathologySession {
    geojson_path: PathBuf,
    mapping_path: PathBuf,
    semantic_digest: String,
    preview: PathologyPreview,
    diagnostic_count: usize,
    editable_ann: Option<AnnotationDocument>,
}

impl PathologySession {
    pub(super) fn preview(&self) -> &PathologyPreview {
        &self.preview
    }

    pub(super) const fn diagnostic_count(&self) -> usize {
        self.diagnostic_count
    }

    pub(super) fn geojson_path(&self) -> &Path {
        &self.geojson_path
    }

    pub(super) fn mapping_path(&self) -> &Path {
        &self.mapping_path
    }

    pub(super) fn semantic_digest(&self) -> &str {
        &self.semantic_digest
    }

    pub(super) const fn editable_ann(&self) -> Option<&AnnotationDocument> {
        self.editable_ann.as_ref()
    }
}

pub(super) enum PathologyEvent {
    Loaded(Box<PathologySession>),
    Failed(String),
    Disconnected,
}

#[derive(Default)]
pub(super) struct PathologyState {
    job: Option<BackgroundWorker<Result<PathologySession, String>>>,
}

impl PathologyState {
    pub(super) fn clear(&mut self) {
        self.job = None;
    }

    #[cfg(test)]
    pub(super) fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    pub(super) fn start_load(
        &mut self,
        source: DicomAnnotationContext,
        geojson_path: PathBuf,
        mapping_path: PathBuf,
        repaint: &egui::Context,
    ) -> Result<(), String> {
        if self.job.is_some() {
            return Err("Another pathology conversion operation is already running.".into());
        }
        let input = PathologyInput {
            source,
            geojson_path,
            mapping_path,
        };
        self.job = Some(
            BackgroundWorker::spawn("dicom-viewer-pathology", repaint, move || {
                load_pathology(input)
            })
            .map_err(|error| format!("could not start pathology worker: {error}"))?,
        );
        Ok(())
    }

    pub(super) fn poll(&mut self) -> Option<PathologyEvent> {
        let result = match self.job.as_ref()?.poll() {
            WorkerPoll::Complete(result) => result,
            WorkerPoll::Pending => return None,
            WorkerPoll::Disconnected => {
                self.job = None;
                return Some(PathologyEvent::Disconnected);
            }
        };
        self.job = None;
        match result {
            Ok(session) => Some(PathologyEvent::Loaded(Box::new(session))),
            Err(error) => Some(PathologyEvent::Failed(error)),
        }
    }
}
