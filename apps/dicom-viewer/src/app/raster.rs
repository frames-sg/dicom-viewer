use std::path::{Path, PathBuf};

use super::background_worker::{BackgroundWorker, WorkerPoll};
use dicom_viewer_core::{DicomAnnotationContext, ParametricMapPreview};
use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};

mod io;
#[cfg(test)]
mod tests;

pub(super) use io::export_raster_cancellable;
use io::load_raster;

pub(super) const DEFAULT_MAX_INSTANCE_BYTES: u64 = 2_000_000_000;

#[derive(Debug, Clone)]
struct RasterInput {
    source: DicomAnnotationContext,
    profile_path: PathBuf,
    raster_path: PathBuf,
}

#[derive(Debug)]
pub(super) struct RasterSession {
    input: RasterInput,
    semantic_digest: String,
    preview: ParametricMapPreview,
    selected_channel_count: usize,
    frame_count: u32,
}

impl RasterSession {
    pub(super) const fn preview(&self) -> &ParametricMapPreview {
        &self.preview
    }

    pub(super) fn semantic_digest(&self) -> &str {
        &self.semantic_digest
    }

    pub(super) const fn selected_channel_count(&self) -> usize {
        self.selected_channel_count
    }

    pub(super) const fn frame_count(&self) -> u32 {
        self.frame_count
    }

    #[cfg(test)]
    pub(super) fn profile_path(&self) -> &Path {
        &self.input.profile_path
    }

    pub(super) fn raster_path(&self) -> &Path {
        &self.input.raster_path
    }

    pub(super) fn load_texture(&self, context: &egui::Context) -> TextureHandle {
        context.load_texture(
            format!("pm-preview-{}", &self.semantic_digest[..12]),
            preview_image(self.preview(), 0.0, 1.0),
            TextureOptions::LINEAR,
        )
    }
}

pub(super) enum RasterEvent {
    Loaded(Box<RasterSession>),
    Failed(String),
    Disconnected,
}

#[derive(Default)]
pub(super) struct RasterState {
    job: Option<BackgroundWorker<Result<RasterSession, String>>>,
}

impl RasterState {
    pub(super) fn clear(&mut self) {
        self.job = None;
    }

    #[cfg(test)]
    pub(super) const fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    pub(super) fn start_load(
        &mut self,
        source: DicomAnnotationContext,
        profile_path: PathBuf,
        raster_path: PathBuf,
        repaint: &egui::Context,
    ) -> Result<(), String> {
        if self.job.is_some() {
            return Err("Another raster conversion operation is already running.".into());
        }
        let input = RasterInput {
            source,
            profile_path,
            raster_path,
        };
        self.job = Some(
            BackgroundWorker::spawn("dicom-viewer-raster", repaint, move || load_raster(input))
                .map_err(|error| format!("could not start raster worker: {error}"))?,
        );
        Ok(())
    }

    pub(super) fn poll(&mut self) -> Option<RasterEvent> {
        let result = match self.job.as_ref()?.poll() {
            WorkerPoll::Complete(result) => result,
            WorkerPoll::Pending => return None,
            WorkerPoll::Disconnected => {
                self.job = None;
                return Some(RasterEvent::Disconnected);
            }
        };
        self.job = None;
        match result {
            Ok(session) => Some(RasterEvent::Loaded(Box::new(session))),
            Err(error) => Some(RasterEvent::Failed(error)),
        }
    }
}

fn preview_image(preview: &ParametricMapPreview, lower: f32, upper: f32) -> ColorImage {
    let (width, height) = preview.dimensions();
    let pixels = preview
        .normalized_values()
        .iter()
        .map(|value| {
            if !value.is_finite() {
                Color32::TRANSPARENT
            } else {
                heat_color(((*value - lower) / (upper - lower)).clamp(0.0, 1.0))
            }
        })
        .collect();
    ColorImage::new([width as usize, height as usize], pixels)
}

fn heat_color(value: f32) -> Color32 {
    const STOPS: [(f32, [u8; 3]); 5] = [
        (0.0, [12, 7, 134]),
        (0.25, [126, 3, 168]),
        (0.5, [203, 71, 119]),
        (0.75, [248, 149, 64]),
        (1.0, [239, 248, 33]),
    ];
    let upper = STOPS
        .iter()
        .position(|(position, _)| value <= *position)
        .unwrap_or(STOPS.len() - 1)
        .max(1);
    let (lower_position, lower_color) = STOPS[upper - 1];
    let (upper_position, upper_color) = STOPS[upper];
    let fraction = (value - lower_position) / (upper_position - lower_position);
    let channel = |index| {
        (f32::from(lower_color[index])
            + fraction * (f32::from(upper_color[index]) - f32::from(lower_color[index])))
        .round() as u8
    };
    Color32::from_rgb(channel(0), channel(1), channel(2))
}
