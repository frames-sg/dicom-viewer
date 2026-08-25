use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui;
use tempfile::Builder;

use super::background_worker::{BackgroundWorker, WorkerPoll};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkspaceExportKind {
    PortableWorkspace,
    SchemeGeoJson,
    CompatibilityGeoJson,
    DicomAnn,
    CompatibilityDicomAnn,
    DicomSeg,
    DicomSr,
    DicomPm,
}

impl WorkspaceExportKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::PortableWorkspace => "portable workspace",
            Self::SchemeGeoJson => "scheme-aware GeoJSON",
            Self::CompatibilityGeoJson => "CellViT compatibility GeoJSON",
            Self::DicomAnn => "DICOM ANN",
            Self::CompatibilityDicomAnn => "tumor-mask compatibility DICOM ANN",
            Self::DicomSeg => "DICOM SEG",
            Self::DicomSr => "DICOM SR",
            Self::DicomPm => "DICOM PM",
        }
    }
}

pub(super) struct WorkspaceExportResult {
    pub(super) destination: PathBuf,
    pub(super) kind: WorkspaceExportKind,
    pub(super) result: Result<(), String>,
}

pub(super) struct WorkspaceExportJob {
    worker: BackgroundWorker<WorkspaceExportResult>,
    cancellation: Arc<AtomicBool>,
}

impl WorkspaceExportJob {
    pub(super) fn spawn(
        destination: PathBuf,
        kind: WorkspaceExportKind,
        repaint: &egui::Context,
        write: impl FnOnce(&Path, &AtomicBool) -> Result<(), String> + Send + 'static,
    ) -> std::io::Result<Self> {
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let temporary = Builder::new()
            .prefix(".frames-export-")
            .tempfile_in(parent)?
            .into_temp_path();
        Self::spawn_published(
            destination,
            kind,
            repaint,
            move |destination, cancellation| {
                write(&temporary, cancellation).and_then(|()| {
                    if cancellation.load(Ordering::Acquire) {
                        return Err("Export cancelled; the destination was not changed.".into());
                    }
                    temporary.persist(destination).map_err(|error| {
                        format!(
                            "Could not publish export to {}: {}",
                            destination.display(),
                            error.error
                        )
                    })?;
                    Ok(())
                })
            },
        )
    }

    pub(super) fn spawn_published(
        destination: PathBuf,
        kind: WorkspaceExportKind,
        repaint: &egui::Context,
        publish: impl FnOnce(&Path, &AtomicBool) -> Result<(), String> + Send + 'static,
    ) -> std::io::Result<Self> {
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let worker = BackgroundWorker::spawn("dicom-viewer-export", repaint, move || {
            let result = if worker_cancellation.load(Ordering::Acquire) {
                Err("Export cancelled before writing.".into())
            } else {
                publish(&destination, &worker_cancellation)
            };
            WorkspaceExportResult {
                destination,
                kind,
                result,
            }
        })?;
        Ok(Self {
            worker,
            cancellation,
        })
    }

    pub(super) fn cancel(&self) {
        self.cancellation.store(true, Ordering::Release);
    }

    pub(super) fn cancellation_requested(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }

    pub(super) fn poll(&self) -> WorkerPoll<WorkspaceExportResult> {
        self.worker.poll()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn failed_export_keeps_existing_destination_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("result.json");
        fs::write(&destination, b"old").unwrap();
        let context = eframe::egui::Context::default();
        let job = WorkspaceExportJob::spawn(
            destination.clone(),
            WorkspaceExportKind::PortableWorkspace,
            &context,
            |temporary, _| {
                fs::write(temporary, b"partial").unwrap();
                Err("synthetic failure".into())
            },
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            match job.poll() {
                WorkerPoll::Complete(result) => break result,
                WorkerPoll::Pending if Instant::now() < deadline => std::thread::yield_now(),
                WorkerPoll::Pending => panic!("export worker did not finish"),
                WorkerPoll::Disconnected => panic!("export worker disconnected"),
            }
        };
        assert!(result.result.is_err());
        assert_eq!(fs::read(destination).unwrap(), b"old");
    }

    #[test]
    fn cancelled_export_never_publishes_partial_output() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("result.json");
        fs::write(&destination, b"old").unwrap();
        let context = eframe::egui::Context::default();
        let ready = Arc::new(Barrier::new(2));
        let worker_ready = Arc::clone(&ready);
        let job = WorkspaceExportJob::spawn(
            destination.clone(),
            WorkspaceExportKind::SchemeGeoJson,
            &context,
            move |temporary, cancellation| {
                fs::write(temporary, b"new").unwrap();
                worker_ready.wait();
                while !cancellation.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                Ok(())
            },
        )
        .unwrap();
        ready.wait();
        job.cancel();

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match job.poll() {
                WorkerPoll::Complete(result) => {
                    assert!(result.result.is_err());
                    break;
                }
                WorkerPoll::Pending if Instant::now() < deadline => std::thread::yield_now(),
                WorkerPoll::Pending => panic!("cancelled export did not finish"),
                WorkerPoll::Disconnected => panic!("export worker disconnected"),
            }
        }
        assert_eq!(fs::read(destination).unwrap(), b"old");
    }
}
