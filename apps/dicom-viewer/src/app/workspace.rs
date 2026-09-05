mod external;
mod external_overlay;
mod geometry;
mod history;
mod overlay;
mod persistence;
mod scheme_library;
mod selection;
mod spatial;
mod tools;
mod transaction;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dicom_viewer_core::{
    annotation_class_concept_key, AnnotationClassGeometry, AnnotationDocument, AnnotationGeometry,
    AnnotationScheme, ExternalLayerKind, ExternalLayerReference, ExternalPromotionSource,
    InteroperabilityDiagnostic, LayerPresentation, Point2, Result as ViewerResult,
    SegmentOperation, SegmentationDocument, SegmentationPrimitive, SegmentationPrimitiveGeometry,
    SourceFrameContext, StructuredReportDocument, TrackingIdentity, VectorFindingGeometry,
    ViewerError, ViewerSourceIdentity, WorkspaceDocument, WorkspaceObjectProvenance,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wsi_dicom_annotations::SegToAnnConversionPolicy;

use super::pathology::PathologySession;
use super::raster::RasterSession;
use super::report::ReportSession;

pub(in crate::app) use external_overlay::draw_external_layer_overlays;
use history::WorkspaceHistory;
pub(in crate::app) use overlay::draw_workspace_overlay;
pub(super) use persistence::{
    AutosaveStatus, RestoredWorkspace, RevisionStore, WorkspaceAutosave, WorkspaceSaveRequest,
};
pub(super) use scheme_library::{SchemeInstallOutcome, SchemeLibrary};
pub(super) use spatial::WorkspaceSpatialIndex;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ActiveTool {
    Pan,
    #[default]
    Select,
    Polygon,
    Brush,
    Point,
    Ruler,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum EditingRepresentation {
    #[default]
    Vector,
    Segmentation,
}

pub(super) enum ExternalLayerPayload {
    Annotation(Arc<AnnotationDocument>),
    ProfiledGeoJson(Arc<PathologySession>),
    Segmentation {
        document: Arc<SegmentationDocument>,
        vector_groups: Option<Arc<[dicom_viewer_core::AnnotationGroup]>>,
    },
    Heatmap {
        session: Arc<RasterSession>,
        texture: eframe::egui::TextureHandle,
    },
    Report(Arc<ReportSession>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalClassDescriptor {
    pub(in crate::app) key: String,
    pub(in crate::app) label: String,
    pub(in crate::app) geometry: AnnotationClassGeometry,
    pub(in crate::app) object_count: usize,
    pub(in crate::app) exact_scheme_class_id: Option<String>,
    pub(in crate::app) editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalObjectDescriptor {
    pub(in crate::app) source_object_id: String,
    pub(in crate::app) label: String,
    pub(in crate::app) class_key: String,
    pub(in crate::app) promoted: bool,
    pub(in crate::app) promotable: bool,
    pub(in crate::app) promotion_block_reason: Option<String>,
}

#[derive(Debug, Clone)]
enum PreparedExternalPromotion {
    Vector {
        geometry: VectorFindingGeometry,
        tracking: Option<TrackingIdentity>,
        source_frame: SourceFrameContext,
    },
    Segment {
        primitives: Vec<SegmentationPrimitive>,
        tracking: Option<TrackingIdentity>,
        source_frame: SourceFrameContext,
    },
    Measurement {
        endpoints: [Point2; 2],
        physical_length_mm: f64,
        tracking: Option<TrackingIdentity>,
        source_frame: SourceFrameContext,
    },
}

fn apply_external_promotion(
    document: &mut WorkspaceDocument,
    vector_layer: Uuid,
    source_layer_id: Uuid,
    source_object_id: String,
    class_id: &str,
    promotion: PreparedExternalPromotion,
) -> ViewerResult<Uuid> {
    match promotion {
        PreparedExternalPromotion::Vector {
            geometry,
            tracking,
            source_frame,
        } => document.promote_vector_finding(
            vector_layer,
            class_id,
            geometry,
            ExternalPromotionSource::new(source_layer_id, source_object_id, tracking, source_frame),
        ),
        PreparedExternalPromotion::Segment {
            primitives,
            tracking,
            source_frame,
        } => {
            let segmentation_layer = document.ensure_manual_segmentation_layer();
            document.promote_segment(
                segmentation_layer,
                class_id,
                primitives,
                ExternalPromotionSource::new(
                    source_layer_id,
                    source_object_id,
                    tracking,
                    source_frame,
                ),
            )
        }
        PreparedExternalPromotion::Measurement {
            endpoints,
            physical_length_mm,
            tracking,
            source_frame,
        } => document.promote_linear_measurement(
            class_id,
            endpoints,
            Some(physical_length_mm),
            ExternalPromotionSource::new(source_layer_id, source_object_id, tracking, source_frame),
        ),
    }
}

#[derive(Debug, Clone)]
struct PreparedExternalObject {
    source_object_id: String,
    label: String,
    class_key: String,
    geometry: Option<AnnotationClassGeometry>,
    promotion: Option<PreparedExternalPromotion>,
    promotion_block_reason: Option<String>,
}

impl ActiveTool {
    pub(super) const ALL: [Self; 6] = [
        Self::Pan,
        Self::Select,
        Self::Polygon,
        Self::Brush,
        Self::Point,
        Self::Ruler,
    ];

    #[must_use]
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Pan => "Pan",
            Self::Select => "Select",
            Self::Polygon => "Polygon",
            Self::Brush => "Brush",
            Self::Point => "Point",
            Self::Ruler => "Ruler",
        }
    }

    #[must_use]
    pub(super) const fn shortcut(self) -> &'static str {
        match self {
            Self::Pan => "Space",
            Self::Select => "V",
            Self::Polygon => "P",
            Self::Brush => "B",
            Self::Point => "K",
            Self::Ruler => "R",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
enum DraftTarget {
    Vector {
        layer_id: Uuid,
    },
    Segment {
        layer_id: Uuid,
        segment_id: Option<Uuid>,
        operation: SegmentOperation,
    },
}

#[derive(Debug, Clone, Copy)]
enum EditableHandle {
    PointFinding,
    PolygonVertex {
        component_index: usize,
        vertex_index: usize,
    },
    MeasurementEndpoint {
        endpoint_index: usize,
    },
    SegmentPrimitivePoint {
        primitive_index: usize,
        point_index: usize,
    },
}

#[derive(Debug)]
struct HandleDrag {
    object_id: Uuid,
    handle: EditableHandle,
    before: Arc<WorkspaceDocument>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DraftInteraction {
    target: DraftTarget,
    class_id: String,
    points: Vec<Point2>,
}

impl DraftInteraction {
    #[must_use]
    pub(super) fn vector_polygon(
        layer_id: Uuid,
        class_id: impl Into<String>,
        points: Vec<Point2>,
    ) -> Self {
        Self {
            target: DraftTarget::Vector { layer_id },
            class_id: class_id.into(),
            points,
        }
    }

    #[must_use]
    pub(super) fn segment_polygon(
        layer_id: Uuid,
        segment_id: Option<Uuid>,
        operation: SegmentOperation,
        class_id: impl Into<String>,
        points: Vec<Point2>,
    ) -> Self {
        Self {
            target: DraftTarget::Segment {
                layer_id,
                segment_id,
                operation,
            },
            class_id: class_id.into(),
            points,
        }
    }

    #[must_use]
    pub(super) fn points(&self) -> &[Point2] {
        &self.points
    }

    pub(super) fn push_point(&mut self, point: Point2) {
        if self.points.last() != Some(&point) {
            self.points.push(point);
        }
    }

    pub(super) fn pop_point(&mut self) -> Option<Point2> {
        self.points.pop()
    }
}

#[derive(Debug)]
enum DraftUndoStep {
    AppendPoint(Point2),
    RestoreDraft(DraftInteraction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolTransitionOutcome {
    Applied,
    BlockedByDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DraftResolution {
    Resume,
    Finish,
    Discard,
}

pub(super) struct WorkspaceRuntime {
    document: Arc<WorkspaceDocument>,
    history: WorkspaceHistory,
    active_tool: ActiveTool,
    region_class_id: String,
    point_class_id: String,
    active_vector_layer: Uuid,
    active_segmentation_layer: Option<Uuid>,
    editing_representation: EditingRepresentation,
    segment_operation: SegmentOperation,
    brush_diameter: f64,
    selection: HashSet<Uuid>,
    draft: Option<DraftInteraction>,
    draft_undo: Vec<DraftUndoStep>,
    pending_tool: Option<ActiveTool>,
    brush_stroke: Option<Vec<Point2>>,
    brush_operation: Option<SegmentOperation>,
    ruler_start: Option<Point2>,
    spatial_index: WorkspaceSpatialIndex,
    spatial_revision: u64,
    external_payloads: HashMap<Uuid, ExternalLayerPayload>,
    handle_drag: Option<HandleDrag>,
}

impl WorkspaceRuntime {
    pub(super) fn set_layer_visibility(
        &mut self,
        layer_id: Uuid,
        visible: bool,
    ) -> ViewerResult<()> {
        let mut presentation = self.document.presentation().layer(layer_id);
        presentation.visible = visible;
        self.set_layer_presentation_without_history(layer_id, presentation)
    }

    pub(super) fn set_layer_presentation_without_history(
        &mut self,
        layer_id: Uuid,
        presentation: LayerPresentation,
    ) -> ViewerResult<()> {
        let mut candidate = (*self.document).clone();
        candidate.set_layer_presentation(layer_id, presentation)?;
        candidate.validate()?;
        self.document = Arc::new(candidate);
        self.invalidate_spatial_index();
        Ok(())
    }

    pub(super) fn set_object_visibility_without_history(
        &mut self,
        object_id: Uuid,
        visible: bool,
    ) -> ViewerResult<()> {
        let mut candidate = (*self.document).clone();
        candidate.set_object_visible(object_id, visible)?;
        candidate.validate()?;
        self.document = Arc::new(candidate);
        self.invalidate_spatial_index();
        Ok(())
    }
}

#[cfg(test)]
mod persistence_tests;
#[cfg(test)]
mod tests;
