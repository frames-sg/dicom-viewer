use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use dicom_viewer_core::{ViewerOpenOptions, ViewerStudy};
use eframe::egui;

#[derive(Debug)]
pub(super) struct OpenJob {
    pub(super) path: PathBuf,
    pub(super) receiver: Receiver<OpenResult>,
}

impl OpenJob {
    pub(super) fn spawn(
        path: PathBuf,
        generation: u64,
        ctx: &egui::Context,
        options: ViewerOpenOptions,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let worker_path = path.clone();
        let repaint = ctx.clone();
        thread::Builder::new()
            .name("dicom-viewer-open".into())
            .spawn(move || {
                let result = ViewerStudy::open_path_with_options(&worker_path, options)
                    .map_err(|err| err.to_string());
                let sent = sender.send(OpenResult {
                    generation,
                    path: worker_path,
                    result,
                });
                if sent.is_ok() {
                    repaint.request_repaint();
                }
            })?;

        Ok(Self { path, receiver })
    }
}

#[derive(Debug)]
pub(super) struct OpenResult {
    pub(super) generation: u64,
    pub(super) path: PathBuf,
    pub(super) result: std::result::Result<ViewerStudy, String>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::OpenJob;

    #[test]
    fn open_worker_reports_invalid_input_without_panicking() {
        let dir = tempfile::tempdir().expect("temporary directory should be created");
        let path = dir.path().join("invalid.dcm");
        std::fs::write(&path, b"not a DICOM file").expect("fixture should be written");
        let context = eframe::egui::Context::default();

        let job = OpenJob::spawn(
            path.clone(),
            7,
            &context,
            dicom_viewer_core::ViewerOpenOptions::cpu_only(),
        )
        .expect("open worker should start");
        let result = job
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("open worker should report a result");

        assert_eq!(result.generation, 7);
        assert_eq!(result.path, path);
        assert!(result.result.is_err());
    }
}
