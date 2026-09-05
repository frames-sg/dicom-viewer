use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;

use super::export_job::WorkspaceExportKind;
use super::workspace_actions::ensure_export_not_cancelled;
use super::DicomViewerApp;

#[derive(Debug)]
struct ScreenshotRequest;

pub(super) struct PendingViewportExport {
    destination: PathBuf,
    canvas_rect: egui::Rect,
    viewport_rect: egui::Rect,
    screenshot_request: egui::UserData,
    request_sent: bool,
}

impl PendingViewportExport {
    fn new(destination: PathBuf, canvas_rect: egui::Rect, viewport_rect: egui::Rect) -> Self {
        Self {
            destination,
            canvas_rect,
            viewport_rect,
            screenshot_request: egui::UserData::new(ScreenshotRequest),
            request_sent: false,
        }
    }

    pub(super) fn update_canvas(&mut self, canvas_rect: egui::Rect, viewport_rect: egui::Rect) {
        self.canvas_rect = canvas_rect;
        self.viewport_rect = viewport_rect;
    }

    fn request_if_needed(&mut self, ctx: &egui::Context) {
        if self.request_sent {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
            self.screenshot_request.clone(),
        ));
        self.request_sent = true;
    }

    fn matching_image(&self, ctx: &egui::Context) -> Option<Arc<egui::ColorImage>> {
        ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot {
                    viewport_id,
                    user_data,
                    image,
                } if *viewport_id == egui::ViewportId::ROOT
                    && user_data == &self.screenshot_request =>
                {
                    Some(Arc::clone(image))
                }
                _ => None,
            })
        })
    }
}

pub(super) fn should_draw_canvas_hud(capture_pending: bool) -> bool {
    !capture_pending
}

#[derive(Debug, PartialEq, Eq)]
struct CapturedView {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl DicomViewerApp {
    pub(super) fn begin_current_view_tiff_export(&mut self, ctx: &egui::Context) {
        if self.workspace_export_job.is_some() || self.pending_viewport_export.is_some() {
            self.status = "Another export is already running.".into();
            return;
        }
        let Some(canvas_rect) = self.last_canvas_rect else {
            self.status = "The current slide view is not ready to capture.".into();
            return;
        };
        let default_name = self
            .active_path
            .as_deref()
            .and_then(Path::file_stem)
            .and_then(|stem| stem.to_str())
            .map_or_else(
                || "current-view.tiff".to_owned(),
                |stem| format!("{stem}-current-view.tiff"),
            );
        let Some(destination) =
            self.choose_export_path("TIFF image", &["tif", "tiff"], &default_name)
        else {
            return;
        };
        self.pending_viewport_export = Some(PendingViewportExport::new(
            destination,
            canvas_rect,
            ctx.viewport_rect(),
        ));
        self.status = "Preparing the current slide view for TIFF export…".into();
        ctx.request_repaint();
    }

    pub(super) fn poll_current_view_tiff_export(&mut self, ctx: &egui::Context) {
        let image = self.pending_viewport_export.as_mut().and_then(|pending| {
            let image = pending.matching_image(ctx);
            pending.request_if_needed(ctx);
            image
        });
        let Some(image) = image else {
            return;
        };
        let pending = self
            .pending_viewport_export
            .take()
            .expect("a matching screenshot requires a pending export");
        let view = match crop_screenshot(image.as_ref(), pending.viewport_rect, pending.canvas_rect)
        {
            Ok(view) => view,
            Err(error) => {
                self.status = format!("Could not capture the current slide view: {error}");
                return;
            }
        };
        self.start_workspace_export(
            pending.destination,
            WorkspaceExportKind::CurrentViewTiff,
            ctx,
            move |temporary, cancellation| {
                ensure_export_not_cancelled(cancellation)?;
                write_tiff(temporary, &view)?;
                ensure_export_not_cancelled(cancellation)
            },
        );
    }
}

fn crop_screenshot(
    screenshot: &egui::ColorImage,
    viewport_rect: egui::Rect,
    canvas_rect: egui::Rect,
) -> Result<CapturedView, String> {
    if screenshot.size[0].checked_mul(screenshot.size[1]) != Some(screenshot.pixels.len()) {
        return Err("the screenshot pixel count does not match its dimensions".into());
    }
    if [
        viewport_rect.min.x,
        viewport_rect.min.y,
        viewport_rect.max.x,
        viewport_rect.max.y,
        canvas_rect.min.x,
        canvas_rect.min.y,
        canvas_rect.max.x,
        canvas_rect.max.y,
    ]
    .iter()
    .any(|value| !value.is_finite())
    {
        return Err("the viewport or canvas bounds are invalid".into());
    }
    if viewport_rect.width() <= 0.0 || viewport_rect.height() <= 0.0 {
        return Err("the viewport dimensions are invalid".into());
    }

    let image_width = screenshot.size[0];
    let image_height = screenshot.size[1];
    let scale_x = image_width as f32 / viewport_rect.width();
    let scale_y = image_height as f32 / viewport_rect.height();
    let min_x = ((canvas_rect.min.x - viewport_rect.min.x) * scale_x)
        .floor()
        .clamp(0.0, image_width as f32) as usize;
    let min_y = ((canvas_rect.min.y - viewport_rect.min.y) * scale_y)
        .floor()
        .clamp(0.0, image_height as f32) as usize;
    let max_x = ((canvas_rect.max.x - viewport_rect.min.x) * scale_x)
        .ceil()
        .clamp(0.0, image_width as f32) as usize;
    let max_y = ((canvas_rect.max.y - viewport_rect.min.y) * scale_y)
        .ceil()
        .clamp(0.0, image_height as f32) as usize;
    if max_x <= min_x || max_y <= min_y {
        return Err("the slide canvas is outside the captured window".into());
    }

    let width = max_x - min_x;
    let height = max_y - min_y;
    let rgb_capacity = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| "the TIFF dimensions overflow memory limits".to_owned())?;
    let mut rgb = Vec::with_capacity(rgb_capacity);
    for y in min_y..max_y {
        let row_start = y * image_width + min_x;
        for pixel in &screenshot.pixels[row_start..row_start + width] {
            let [red, green, blue, _] = pixel.to_array();
            rgb.extend_from_slice(&[red, green, blue]);
        }
    }
    Ok(CapturedView {
        width: u32::try_from(width)
            .map_err(|_| "the TIFF width exceeds the supported range".to_owned())?,
        height: u32::try_from(height)
            .map_err(|_| "the TIFF height exceeds the supported range".to_owned())?,
        rgb,
    })
}

fn write_tiff(path: &Path, view: &CapturedView) -> Result<(), String> {
    let expected = usize::try_from(view.width)
        .ok()
        .and_then(|width| {
            usize::try_from(view.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| "the TIFF dimensions overflow memory limits".to_owned())?;
    if view.rgb.len() != expected {
        return Err("the TIFF pixel count does not match its dimensions".into());
    }
    let mut file = File::create(path)
        .map_err(|error| format!("Could not create TIFF temporary file: {error}"))?;
    {
        let mut encoder = tiff::encoder::TiffEncoder::new(&mut file)
            .map_err(|error| format!("Could not initialize TIFF encoder: {error}"))?;
        encoder
            .write_image::<tiff::encoder::colortype::RGB8>(view.width, view.height, &view.rgb)
            .map_err(|error| format!("Could not encode TIFF pixels: {error}"))?;
    }
    file.sync_all()
        .map_err(|error| format!("Could not flush TIFF output: {error}"))
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Color32};

    use super::*;

    #[test]
    fn screenshot_crop_uses_physical_pixels_and_excludes_surrounding_ui() {
        let pixels = (0_u8..48)
            .map(|value| Color32::from_rgb(value, value.saturating_add(1), value.saturating_add(2)))
            .collect();
        let screenshot = egui::ColorImage::new([8, 6], pixels);
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(4.0, 3.0));
        let canvas = egui::Rect::from_min_max(egui::pos2(1.0, 1.0), egui::pos2(3.0, 2.0));

        let cropped = crop_screenshot(&screenshot, viewport, canvas).unwrap();

        assert_eq!((cropped.width, cropped.height), (4, 2));
        assert_eq!(cropped.rgb.len(), 4 * 2 * 3);
        assert_eq!(cropped.rgb[0..3], [18, 19, 20]);
        assert_eq!(cropped.rgb[cropped.rgb.len() - 3..], [29, 30, 31]);
    }

    #[test]
    fn screenshot_crop_is_relative_to_the_native_viewport_origin() {
        let pixels = (0_u8..48)
            .map(|value| Color32::from_rgb(value, value, value))
            .collect();
        let screenshot = egui::ColorImage::new([8, 6], pixels);
        let viewport = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(4.0, 3.0));
        let canvas = egui::Rect::from_min_max(egui::pos2(101.0, 51.0), egui::pos2(103.0, 52.0));

        let cropped = crop_screenshot(&screenshot, viewport, canvas).unwrap();

        assert_eq!((cropped.width, cropped.height), (4, 2));
        assert_eq!(cropped.rgb[0..3], [18, 18, 18]);
        assert_eq!(cropped.rgb[cropped.rgb.len() - 3..], [29, 29, 29]);
    }

    #[test]
    fn current_view_capture_hides_the_diagnostic_canvas_hud() {
        assert!(should_draw_canvas_hud(false));
        assert!(!should_draw_canvas_hud(true));
    }

    #[test]
    fn current_view_writer_emits_a_readable_rgb_tiff() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("view.tiff");
        let view = CapturedView {
            width: 2,
            height: 1,
            rgb: vec![255, 0, 0, 0, 127, 255],
        };

        write_tiff(&path, &view).unwrap();

        let mut decoder = tiff::decoder::Decoder::new(std::fs::File::open(path).unwrap()).unwrap();
        assert_eq!(decoder.dimensions().unwrap(), (2, 1));
        assert_eq!(decoder.colortype().unwrap(), tiff::ColorType::RGB(8));
        let tiff::decoder::DecodingResult::U8(decoded) = decoder.read_image().unwrap() else {
            panic!("RGB8 TIFF should decode to eight-bit samples");
        };
        assert_eq!(decoded, view.rgb);
    }
}
