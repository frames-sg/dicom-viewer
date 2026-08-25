use std::collections::BTreeSet;
use std::path::PathBuf;

use super::background_worker::{BackgroundWorker, WorkerPoll};
use super::viewport::point_bounds;
use dicom_viewer_core::{
    CoordinateGraphic, DicomAnnotationContext, Point2, SegmentationDocument, SegmentationKind,
    StructuredReportDocument,
};
use eframe::egui;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(super) struct ReportRegion {
    graphic: CoordinateGraphic,
    points: Vec<Point2>,
    bounds: [f64; 4],
}

impl ReportRegion {
    pub(super) const fn graphic(&self) -> CoordinateGraphic {
        self.graphic
    }

    pub(super) fn points(&self) -> &[Point2] {
        &self.points
    }

    pub(super) const fn bounds(&self) -> [f64; 4] {
        self.bounds
    }
}

#[derive(Debug)]
pub(super) struct ReportMaskRun {
    pub(super) row: u32,
    pub(super) column_start: u32,
    pub(super) values: ReportMaskValues,
    pub(super) color: [u16; 3],
}

#[derive(Debug)]
pub(super) enum ReportMaskValues {
    Binary(u32),
    Fractional { maximum: u16, values: Vec<u16> },
}

#[derive(Debug)]
pub(super) struct ReportSession {
    document: StructuredReportDocument,
    regions: Vec<ReportRegion>,
    mask_runs: Vec<ReportMaskRun>,
}

impl ReportSession {
    #[cfg(test)]
    pub(super) fn from_document(document: StructuredReportDocument) -> Result<Self, String> {
        let regions = collect_regions(&document, document.source())?;
        Ok(Self {
            document,
            regions,
            mask_runs: Vec::new(),
        })
    }

    pub(super) const fn document(&self) -> &StructuredReportDocument {
        &self.document
    }

    pub(super) fn regions(&self) -> &[ReportRegion] {
        &self.regions
    }

    pub(super) fn mask_runs(&self) -> &[ReportMaskRun] {
        &self.mask_runs
    }
}

pub(super) enum ReportEvent {
    Loaded {
        path: PathBuf,
        group_count: usize,
        session: Box<ReportSession>,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
    Disconnected,
}

struct ReportLoadResult {
    path: PathBuf,
    result: Result<ReportSession, String>,
}

#[derive(Default)]
pub(super) struct ReportState {
    job: Option<BackgroundWorker<ReportLoadResult>>,
}

impl ReportState {
    pub(super) fn clear(&mut self) {
        self.job = None;
    }

    #[cfg(test)]
    pub(super) const fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    pub(super) fn start_load(
        &mut self,
        path: PathBuf,
        companion_seg_path: Option<PathBuf>,
        context: DicomAnnotationContext,
        repaint: &egui::Context,
    ) -> Result<(), String> {
        if self.job.is_some() {
            return Err("Another structured report is already loading.".into());
        }
        let result_path = path.clone();
        self.job = Some(
            BackgroundWorker::spawn("dicom-viewer-sr-load", repaint, move || ReportLoadResult {
                path: result_path,
                result: load_report(path, companion_seg_path, &context),
            })
            .map_err(|error| format!("could not start structured report worker: {error}"))?,
        );
        Ok(())
    }

    pub(super) fn poll(&mut self) -> Option<ReportEvent> {
        let result = match self.job.as_ref()?.poll() {
            WorkerPoll::Complete(result) => result,
            WorkerPoll::Pending => return None,
            WorkerPoll::Disconnected => {
                self.job = None;
                return Some(ReportEvent::Disconnected);
            }
        };
        self.job = None;
        match result.result {
            Ok(session) => {
                let group_count = session.document.groups().len();
                Some(ReportEvent::Loaded {
                    path: result.path,
                    group_count,
                    session: Box::new(session),
                })
            }
            Err(error) => Some(ReportEvent::Failed {
                path: result.path,
                error,
            }),
        }
    }
}

fn load_report(
    path: PathBuf,
    companion_seg_path: Option<PathBuf>,
    context: &DicomAnnotationContext,
) -> Result<ReportSession, String> {
    let segmentation = companion_seg_path
        .as_ref()
        .map(|path| {
            SegmentationDocument::read_seg(path, context).map_err(|error| error.to_string())
        })
        .transpose()?;
    let document = StructuredReportDocument::read_sr(&path, context, segmentation.as_ref())
        .map_err(|error| {
            if companion_seg_path.is_none() {
                format!(
                    "{error}. If this SR references SEG, use ‘Open SR + SEG’ and select its companion."
                )
            } else {
                error.to_string()
            }
        })?;
    let regions = collect_regions(&document, context)?;
    let mask_runs = segmentation
        .as_ref()
        .map(|segmentation| collect_mask_runs(&document, segmentation))
        .transpose()?
        .unwrap_or_default();
    Ok(ReportSession {
        document,
        regions,
        mask_runs,
    })
}

fn collect_regions(
    document: &StructuredReportDocument,
    context: &DicomAnnotationContext,
) -> Result<Vec<ReportRegion>, String> {
    let mut regions = Vec::new();
    for group in document.groups() {
        let coordinates = group.region_coordinates().into_iter().chain(
            group
                .measurements()
                .iter()
                .flat_map(|measurement| measurement.coordinates()),
        );
        for coordinates in coordinates {
            let points = coordinates
                .points()
                .iter()
                .map(|point| {
                    context
                        .slide_coordinate_to_pixel3(point.x, point.y, point.z)
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            regions.push(ReportRegion {
                graphic: coordinates.graphic(),
                bounds: point_bounds(&points),
                points,
            });
        }
    }
    Ok(regions)
}

fn collect_mask_runs(
    report: &StructuredReportDocument,
    segmentation: &SegmentationDocument,
) -> Result<Vec<ReportMaskRun>, String> {
    let referenced = report
        .groups()
        .iter()
        .filter_map(|group| group.referenced_segment_number())
        .collect::<BTreeSet<_>>();
    if referenced.is_empty() {
        return Ok(Vec::new());
    }
    let color = |segment_number| {
        segmentation
            .segment_for_number(segment_number)
            .map_or([39321, 38036, 35466], |segment| {
                segment.recommended_display_cielab()
            })
    };
    let runs = match segmentation.kind() {
        SegmentationKind::Binary | SegmentationKind::LabelMap => segmentation
            .binary_runs()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|run| referenced.contains(&run.segment_number()))
            .map(|run| ReportMaskRun {
                row: run.row(),
                column_start: run.column_start(),
                values: ReportMaskValues::Binary(run.length()),
                color: color(run.segment_number()),
            })
            .collect(),
        SegmentationKind::Fractional => segmentation
            .fractional_runs()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|run| referenced.contains(&run.segment_number()))
            .map(|run| ReportMaskRun {
                row: run.row(),
                column_start: run.column_start(),
                values: ReportMaskValues::Fractional {
                    maximum: run.maximum_fractional_value(),
                    values: run.values().to_vec(),
                },
                color: color(run.segment_number()),
            })
            .collect(),
    };
    Ok(runs)
}
