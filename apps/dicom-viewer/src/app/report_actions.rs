use eframe::egui;

use super::report::ReportEvent;
use super::DicomViewerApp;

impl DicomViewerApp {
    pub(super) fn poll_report_job(&mut self) {
        let Some(event) = self.report.poll() else {
            return;
        };
        self.status =
            match event {
                ReportEvent::Loaded {
                    path,
                    group_count,
                    session,
                } => {
                    let name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Imported SR")
                        .to_owned();
                    match self.workspace.as_mut().map(|runtime| {
                        runtime.add_external_report(name, Some(path.clone()), *session)
                    }) {
                        Some(Ok(_)) => format!(
                            "Loaded {group_count} SR measurement group(s) from {}.",
                            path.display()
                        ),
                        Some(Err(error)) => {
                            format!("Loaded SR but could not register its source layer: {error}")
                        }
                        None => "The pathology workspace is unavailable.".into(),
                    }
                }
                ReportEvent::Failed { path, error } => {
                    format!(
                        "Failed to load structured report {}: {error}",
                        path.display()
                    )
                }
                ReportEvent::Disconnected => {
                    "Structured report worker exited without returning a result.".into()
                }
            };
    }

    pub(super) fn pick_structured_report(&mut self, ctx: &egui::Context, with_seg: bool) {
        let Some(context) = self
            .study
            .as_ref()
            .and_then(|study| study.annotation_context())
            .cloned()
        else {
            self.status = "Structured reports require an open VL WSI DICOM source.".into();
            return;
        };
        let mut report_dialog = rfd::FileDialog::new().add_filter("DICOM SR", &["dcm", "dicom"]);
        if let Some(directory) = context.source_path().parent() {
            report_dialog = report_dialog.set_directory(directory);
        }
        let Some(report_path) = report_dialog.pick_file() else {
            return;
        };
        let companion_seg_path = if with_seg {
            let mut seg_dialog = rfd::FileDialog::new().add_filter("DICOM SEG", &["dcm", "dicom"]);
            if let Some(directory) = report_path.parent() {
                seg_dialog = seg_dialog.set_directory(directory);
            }
            let Some(seg_path) = seg_dialog.pick_file() else {
                self.status =
                    "Structured report import cancelled; no companion SEG selected.".into();
                return;
            };
            Some(seg_path)
        } else {
            None
        };
        self.start_structured_report_path(report_path, companion_seg_path, ctx);
    }

    pub(super) fn start_structured_report_path(
        &mut self,
        report_path: std::path::PathBuf,
        companion_seg_path: Option<std::path::PathBuf>,
        ctx: &egui::Context,
    ) {
        let Some(context) = self
            .study
            .as_ref()
            .and_then(|study| study.annotation_context())
            .cloned()
        else {
            self.status = "Structured reports require an open VL WSI DICOM source.".into();
            return;
        };
        match self
            .report
            .start_load(report_path.clone(), companion_seg_path, context, ctx)
        {
            Ok(()) => self.status = format!("Reading structured report {}…", report_path.display()),
            Err(error) => self.status = error,
        }
    }
}
