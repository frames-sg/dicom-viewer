use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use dicom_viewer_core::{ViewerOpenOptions, ViewerStudy};
use eframe::egui;

const MAX_OPEN_WORKERS: usize = 2;

#[derive(Debug)]
struct PendingOpen {
    path: PathBuf,
    generation: u64,
    options: ViewerOpenOptions,
}

#[derive(Debug, Default)]
pub(super) struct OpenQueue {
    active: Vec<OpenJob>,
    pending: Option<PendingOpen>,
}

pub(super) enum OpenPoll {
    Result(Box<OpenResult>),
    Disconnected(PathBuf),
}

impl OpenQueue {
    pub(super) fn submit(
        &mut self,
        path: PathBuf,
        generation: u64,
        ctx: &egui::Context,
        options: ViewerOpenOptions,
    ) -> std::io::Result<()> {
        if self.active.len() < MAX_OPEN_WORKERS {
            self.active
                .push(OpenJob::spawn(path, generation, ctx, options)?);
        } else {
            self.pending = Some(PendingOpen {
                path,
                generation,
                options,
            });
        }
        Ok(())
    }

    pub(super) fn poll(&mut self, ctx: &egui::Context) -> std::io::Result<Option<OpenPoll>> {
        let ready =
            self.active
                .iter()
                .enumerate()
                .find_map(|(index, job)| match job.receiver.try_recv() {
                    Ok(result) => Some((index, OpenPoll::Result(Box::new(result)))),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Some((index, OpenPoll::Disconnected(job.path.clone())))
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                });
        let Some((index, poll)) = ready else {
            return Ok(None);
        };
        self.active.swap_remove(index);
        self.start_pending_if_possible(ctx)?;
        Ok(Some(poll))
    }

    pub(super) fn is_opening(&self) -> bool {
        !self.active.is_empty() || self.pending.is_some()
    }

    fn start_pending_if_possible(&mut self, ctx: &egui::Context) -> std::io::Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        self.active.push(OpenJob::spawn(
            pending.path,
            pending.generation,
            ctx,
            pending.options,
        )?);
        Ok(())
    }
}

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

    use super::{OpenJob, OpenQueue};

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

    #[test]
    fn open_queue_limits_workers_and_keeps_only_the_latest_pending_request() {
        let context = eframe::egui::Context::default();
        let mut queue = OpenQueue::default();
        let dir = tempfile::tempdir().expect("temporary directory should be created");

        for generation in 1..=4 {
            let path = dir.path().join(format!("invalid-{generation}.dcm"));
            std::fs::write(&path, b"not a DICOM file").expect("fixture should be written");
            queue
                .submit(
                    path,
                    generation,
                    &context,
                    dicom_viewer_core::ViewerOpenOptions::cpu_only(),
                )
                .expect("open request should be accepted");
        }

        assert_eq!(queue.active.len(), 2);
        assert_eq!(
            queue
                .pending
                .as_ref()
                .expect("latest request should be pending")
                .generation,
            4
        );
    }
}
