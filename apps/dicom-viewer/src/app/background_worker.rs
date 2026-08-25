use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui;

pub(super) enum WorkerPoll<T> {
    Pending,
    Complete(T),
    Disconnected,
}

pub(super) struct BackgroundWorker<T> {
    receiver: Receiver<T>,
}

impl<T: Send + 'static> BackgroundWorker<T> {
    pub(super) fn spawn(
        name: &str,
        repaint: &egui::Context,
        run: impl FnOnce() -> T + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let repaint = repaint.clone();
        thread::Builder::new().name(name.into()).spawn(move || {
            if sender.send(run()).is_ok() {
                repaint.request_repaint();
            }
        })?;
        Ok(Self { receiver })
    }

    pub(super) fn poll(&self) -> WorkerPoll<T> {
        match self.receiver.try_recv() {
            Ok(result) => WorkerPoll::Complete(result),
            Err(TryRecvError::Empty) => WorkerPoll::Pending,
            Err(TryRecvError::Disconnected) => WorkerPoll::Disconnected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::wait_for_background;

    #[test]
    fn worker_distinguishes_pending_completion_and_disconnection() {
        let (release_sender, release_receiver) = mpsc::channel();
        let worker = BackgroundWorker::spawn(
            "dicom-viewer-background-worker-test",
            &egui::Context::default(),
            move || {
                release_receiver.recv().unwrap();
                17_u8
            },
        )
        .unwrap();

        assert!(matches!(worker.poll(), WorkerPoll::Pending));
        release_sender.send(()).unwrap();
        let value = wait_for_background("background worker", || match worker.poll() {
            WorkerPoll::Complete(value) => Some(value),
            WorkerPoll::Pending => None,
            WorkerPoll::Disconnected => panic!("worker disconnected before returning its value"),
        });
        assert_eq!(value, 17);
        wait_for_background("background worker disconnect", || match worker.poll() {
            WorkerPoll::Disconnected => Some(()),
            WorkerPoll::Pending => None,
            WorkerPoll::Complete(_) => panic!("worker returned more than one value"),
        });
    }
}
