mod external_overlay;
mod history;
mod overlay;
mod persistence;
mod scheme_library;
mod spatial;

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
    pub(super) fn new(
        source_identity: ViewerSourceIdentity,
        scheme: AnnotationScheme,
    ) -> ViewerResult<Self> {
        Self::from_document(WorkspaceDocument::new(source_identity, scheme)?)
    }

    #[cfg(test)]
    pub(super) fn with_history_limits(
        source_identity: ViewerSourceIdentity,
        scheme: AnnotationScheme,
        max_commands: usize,
        max_retained_bytes: usize,
    ) -> ViewerResult<Self> {
        let mut runtime = Self::new(source_identity, scheme)?;
        runtime.history = WorkspaceHistory::with_limits(max_commands, max_retained_bytes);
        Ok(runtime)
    }

    pub(super) fn from_document(document: WorkspaceDocument) -> ViewerResult<Self> {
        document.validate()?;
        let active_vector_layer = document.vector_layers()[0].id();
        let document = Arc::new(document);
        let spatial_index = WorkspaceSpatialIndex::build(&document)?;
        let spatial_revision = document.revision();
        let region_class_id = document
            .scheme()
            .classes()
            .iter()
            .find(|class| class.geometry() == dicom_viewer_core::AnnotationClassGeometry::Region)
            .map(|class| class.id().to_owned())
            .ok_or_else(|| ViewerError::InvalidInput("scheme has no region class".into()))?;
        let point_class_id = document
            .scheme()
            .classes()
            .iter()
            .find(|class| class.geometry() == dicom_viewer_core::AnnotationClassGeometry::Point)
            .map(|class| class.id().to_owned())
            .unwrap_or_else(|| region_class_id.clone());
        Ok(Self {
            document,
            history: WorkspaceHistory::default(),
            active_tool: ActiveTool::Select,
            region_class_id,
            point_class_id,
            active_vector_layer,
            active_segmentation_layer: None,
            editing_representation: EditingRepresentation::Vector,
            segment_operation: SegmentOperation::Add,
            brush_diameter: 40.0,
            selection: HashSet::new(),
            draft: None,
            draft_undo: Vec::new(),
            pending_tool: None,
            brush_stroke: None,
            brush_operation: None,
            ruler_start: None,
            spatial_index,
            spatial_revision,
            external_payloads: HashMap::new(),
            handle_drag: None,
        })
    }

    #[must_use]
    pub(super) fn document(&self) -> &WorkspaceDocument {
        &self.document
    }

    #[must_use]
    pub(super) fn document_snapshot(&self) -> Arc<WorkspaceDocument> {
        Arc::clone(&self.document)
    }

    pub(super) fn edit<T>(
        &mut self,
        label: impl Into<String>,
        edit: impl FnOnce(&mut WorkspaceDocument) -> ViewerResult<T>,
    ) -> ViewerResult<T> {
        let before = Arc::clone(&self.document);
        let mut candidate = (*before).clone();
        let result = edit(&mut candidate)?;
        candidate.validate()?;
        if candidate.revision() != before.revision() {
            let after = Arc::new(candidate);
            self.history
                .record(label, Arc::clone(&before), Arc::clone(&after));
            self.document = after;
            self.draft_undo.clear();
            self.invalidate_spatial_index();
        }
        Ok(result)
    }

    pub(super) fn undo(&mut self) -> bool {
        let Some(document) = self.history.undo() else {
            return false;
        };
        self.document = document;
        self.selection.retain(|id| {
            self.document.finding(*id).is_some()
                || self.document.segment(*id).is_some()
                || self.document.measurement(*id).is_some()
        });
        self.invalidate_spatial_index();
        true
    }

    pub(super) fn redo(&mut self) -> bool {
        let Some(document) = self.history.redo() else {
            return false;
        };
        self.document = document;
        self.invalidate_spatial_index();
        true
    }

    #[must_use]
    pub(super) fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    #[must_use]
    pub(super) fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    #[must_use]
    pub(super) fn history_truncated(&self) -> bool {
        self.history.truncated()
    }

    #[must_use]
    pub(super) fn undo_label(&self) -> Option<&str> {
        self.history.undo_label()
    }

    #[must_use]
    pub(super) fn redo_label(&self) -> Option<&str> {
        self.history.redo_label()
    }

    #[must_use]
    pub(super) const fn active_tool(&self) -> ActiveTool {
        self.active_tool
    }

    #[cfg(test)]
    pub(super) fn set_active_tool(&mut self, tool: ActiveTool) -> ViewerResult<()> {
        match self.request_tool(tool) {
            ToolTransitionOutcome::Applied => Ok(()),
            ToolTransitionOutcome::BlockedByDraft => Err(ViewerError::InvalidInput(
                "finish, resume, or discard the current polygon before switching tools".into(),
            )),
        }
    }

    pub(super) fn request_tool(&mut self, tool: ActiveTool) -> ToolTransitionOutcome {
        if self.active_tool == tool {
            return ToolTransitionOutcome::Applied;
        }
        if self.draft.is_some() {
            self.pending_tool = Some(tool);
            return ToolTransitionOutcome::BlockedByDraft;
        }
        self.active_tool = tool;
        self.draft_undo.clear();
        if matches!(tool, ActiveTool::Point | ActiveTool::Ruler) {
            self.editing_representation = EditingRepresentation::Vector;
        }
        self.pending_tool = None;
        ToolTransitionOutcome::Applied
    }

    pub(super) fn resolve_draft_transition(
        &mut self,
        resolution: DraftResolution,
    ) -> ViewerResult<()> {
        match resolution {
            DraftResolution::Resume => {
                self.pending_tool = None;
            }
            DraftResolution::Finish => {
                self.finish_draft()?;
                if let Some(tool) = self.pending_tool.take() {
                    self.active_tool = tool;
                }
            }
            DraftResolution::Discard => {
                self.draft = None;
                self.draft_undo.clear();
                if let Some(tool) = self.pending_tool.take() {
                    self.active_tool = tool;
                }
            }
        }
        Ok(())
    }

    pub(super) fn finish_draft(&mut self) -> ViewerResult<Uuid> {
        let draft = self
            .draft
            .clone()
            .ok_or_else(|| ViewerError::InvalidInput("there is no polygon draft".into()))?;
        let target_layer = match draft.target {
            DraftTarget::Vector { layer_id } | DraftTarget::Segment { layer_id, .. } => layer_id,
        };
        self.ensure_layer_editable(target_layer)?;
        let result = match draft.target {
            DraftTarget::Vector { layer_id } => self.edit("Add polygon finding", |document| {
                document.add_vector_finding(
                    layer_id,
                    &draft.class_id,
                    dicom_viewer_core::VectorFindingGeometry::regions(vec![draft.points]),
                )
            })?,
            DraftTarget::Segment {
                layer_id,
                segment_id,
                operation,
            } => {
                let primitive = SegmentationPrimitive::polygon(operation, draft.points);
                if let Some(segment_id) = segment_id {
                    self.edit("Edit segment", |document| {
                        document.apply_segment_primitive(segment_id, primitive)?;
                        Ok(segment_id)
                    })?
                } else {
                    self.edit("Add segment", |document| {
                        document.add_segment(layer_id, &draft.class_id, primitive)
                    })?
                }
            }
        };
        self.draft = None;
        self.select_only(result);
        Ok(result)
    }

    #[must_use]
    pub(super) fn draft(&self) -> Option<&DraftInteraction> {
        self.draft.as_ref()
    }

    pub(super) fn set_draft(&mut self, draft: DraftInteraction) {
        self.draft = Some(draft);
        self.draft_undo.clear();
    }

    pub(super) fn add_polygon_point(&mut self, point: Point2) -> ViewerResult<()> {
        self.draft_undo.clear();
        if let Some(draft) = &mut self.draft {
            draft.push_point(point);
            return Ok(());
        }
        let draft = match self.editing_representation {
            EditingRepresentation::Vector => {
                self.ensure_layer_editable(self.active_vector_layer)?;
                DraftInteraction::vector_polygon(
                    self.active_vector_layer,
                    self.region_class_id.clone(),
                    vec![point],
                )
            }
            EditingRepresentation::Segmentation => {
                let layer_id = self.ensure_segmentation_layer()?;
                self.ensure_layer_editable(layer_id)?;
                let segment_id = self.selected_segment();
                if self.segment_operation == SegmentOperation::Erase && segment_id.is_none() {
                    return Err(ViewerError::InvalidInput(
                        "select a segment before using Erase".into(),
                    ));
                }
                DraftInteraction::segment_polygon(
                    layer_id,
                    segment_id,
                    self.segment_operation,
                    self.region_class_id.clone(),
                    vec![point],
                )
            }
        };
        self.draft = Some(draft);
        Ok(())
    }

    pub(super) fn add_point_finding(&mut self, point: Point2) -> ViewerResult<Uuid> {
        let layer = self.active_vector_layer;
        self.ensure_layer_editable(layer)?;
        let class_id = self.point_class_id.clone();
        let id = self.edit("Add point finding", |document| {
            document.add_vector_finding(
                layer,
                &class_id,
                dicom_viewer_core::VectorFindingGeometry::Point(point),
            )
        })?;
        self.select_only(id);
        Ok(id)
    }

    pub(super) fn begin_brush_stroke_with_operation(
        &mut self,
        point: Point2,
        operation: SegmentOperation,
    ) -> ViewerResult<()> {
        let layer = self.ensure_segmentation_layer()?;
        self.ensure_layer_editable(layer)?;
        if operation == SegmentOperation::Erase && self.selected_segment().is_none() {
            return Err(ViewerError::InvalidInput(
                "select a segment before using Erase".into(),
            ));
        }
        self.brush_stroke = Some(vec![point]);
        self.brush_operation = Some(operation);
        Ok(())
    }

    pub(super) fn extend_brush_stroke(&mut self, point: Point2) {
        if let Some(stroke) = &mut self.brush_stroke {
            let minimum_step = (self.brush_diameter * 0.08).max(0.5);
            if stroke.last().is_none_or(|last| {
                let dx = point.x - last.x;
                let dy = point.y - last.y;
                dx.hypot(dy) >= minimum_step
            }) {
                stroke.push(point);
            }
        }
    }

    pub(super) fn finish_brush_stroke(&mut self) -> ViewerResult<Option<Uuid>> {
        let Some(centerline) = self.brush_stroke.take() else {
            return Ok(None);
        };
        let layer_id = self.ensure_segmentation_layer()?;
        self.ensure_layer_editable(layer_id)?;
        let operation = self
            .brush_operation
            .take()
            .unwrap_or(self.segment_operation);
        let segment_id = self.selected_segment();
        let primitive = SegmentationPrimitive::brush(operation, centerline, self.brush_diameter);
        let id = if let Some(segment_id) = segment_id {
            let outcome = self.edit("Brush stroke", |document| {
                document.apply_segment_primitive(segment_id, primitive)
            })?;
            if outcome == dicom_viewer_core::SegmentEditOutcome::NoIntersection {
                return Ok(None);
            }
            segment_id
        } else {
            let class_id = self.region_class_id.clone();
            self.edit("Add segment", |document| {
                document.add_segment(layer_id, &class_id, primitive)
            })?
        };
        self.select_only(id);
        Ok(Some(id))
    }

    pub(super) fn cancel_pointer_interaction(&mut self) {
        self.brush_stroke = None;
        self.brush_operation = None;
    }

    #[must_use]
    pub(super) fn brush_stroke(&self) -> Option<&[Point2]> {
        self.brush_stroke.as_deref()
    }

    pub(super) fn place_ruler_point(
        &mut self,
        point: Point2,
        physical_length_mm: impl FnOnce(Point2, Point2) -> Option<f64>,
    ) -> ViewerResult<Option<Uuid>> {
        let Some(start) = self.ruler_start.take() else {
            self.ruler_start = Some(point);
            return Ok(None);
        };
        let class_id = self.region_class_id.clone();
        let length = physical_length_mm(start, point);
        let id = self.edit("Add ruler", |document| {
            document.add_linear_measurement(&class_id, [start, point], length)
        })?;
        self.select_only(id);
        Ok(Some(id))
    }

    #[must_use]
    pub(super) const fn ruler_start(&self) -> Option<Point2> {
        self.ruler_start
    }

    pub(super) fn cancel_ruler(&mut self) -> bool {
        self.ruler_start.take().is_some()
    }

    pub(super) fn cancel_draft_step(&mut self) -> bool {
        let Some(before) = self.draft.as_ref().cloned() else {
            return false;
        };
        if before.points().is_empty() {
            self.draft = None;
            return true;
        }
        if before.points().len() == 1 {
            self.draft = None;
            self.draft_undo.push(DraftUndoStep::RestoreDraft(before));
            return true;
        }
        let point = self
            .draft
            .as_mut()
            .and_then(DraftInteraction::pop_point)
            .expect("the non-empty polygon draft was checked");
        self.draft_undo.push(DraftUndoStep::AppendPoint(point));
        true
    }

    pub(super) fn undo_draft_cancel(&mut self) -> bool {
        let Some(step) = self.draft_undo.pop() else {
            return false;
        };
        match step {
            DraftUndoStep::AppendPoint(point) => {
                let Some(draft) = &mut self.draft else {
                    self.draft_undo.push(DraftUndoStep::AppendPoint(point));
                    return false;
                };
                draft.push_point(point);
            }
            DraftUndoStep::RestoreDraft(draft) => {
                if self.draft.is_some() {
                    self.draft_undo.push(DraftUndoStep::RestoreDraft(draft));
                    return false;
                }
                self.draft = Some(draft);
            }
        }
        true
    }

    pub(super) fn discard_draft(&mut self) {
        self.draft = None;
        self.draft_undo.clear();
        self.pending_tool = None;
    }

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

    #[must_use]
    pub(super) fn selection(&self) -> &HashSet<Uuid> {
        &self.selection
    }

    pub(super) fn select_only(&mut self, object_id: Uuid) {
        self.selection.clear();
        self.selection.insert(object_id);
    }

    pub(super) fn toggle_selection(&mut self, object_id: Uuid) {
        if !self.selection.remove(&object_id) {
            self.selection.insert(object_id);
        }
    }

    pub(super) fn clear_selection(&mut self) {
        self.selection.clear();
    }

    fn selected_segment(&self) -> Option<Uuid> {
        self.selection
            .iter()
            .copied()
            .find(|id| self.document.segment(*id).is_some())
    }

    pub(super) fn delete_selection(&mut self) -> ViewerResult<usize> {
        let ids = self.selection.iter().copied().collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(0);
        }
        self.ensure_objects_editable(&ids)?;
        let deleted = self.edit("Delete selection", |document| {
            let mut count = 0;
            for id in &ids {
                count += usize::from(document.delete_object(*id)?);
            }
            Ok(count)
        })?;
        self.clear_selection();
        Ok(deleted)
    }

    pub(super) fn begin_handle_drag(&mut self, point: Point2, tolerance: f64) -> bool {
        let tolerance_squared = tolerance * tolerance;
        let mut closest: Option<(f64, Uuid, EditableHandle)> = None;
        for id in self.selection.iter().copied() {
            if self.object_locked(id) {
                continue;
            }
            if let Some(finding) = self.document.finding(id) {
                match finding.geometry() {
                    VectorFindingGeometry::Point(candidate) => update_handle_candidate(
                        &mut closest,
                        point,
                        *candidate,
                        tolerance_squared,
                        id,
                        EditableHandle::PointFinding,
                    ),
                    VectorFindingGeometry::Regions(components) => {
                        for (component_index, component) in components.iter().enumerate() {
                            for (vertex_index, candidate) in component.iter().copied().enumerate() {
                                update_handle_candidate(
                                    &mut closest,
                                    point,
                                    candidate,
                                    tolerance_squared,
                                    id,
                                    EditableHandle::PolygonVertex {
                                        component_index,
                                        vertex_index,
                                    },
                                );
                            }
                        }
                    }
                }
            } else if let Some(segment) = self.document.segment(id) {
                for (primitive_index, primitive) in segment.primitives().iter().enumerate() {
                    let points = match primitive.geometry() {
                        SegmentationPrimitiveGeometry::Polygon { points } => points.as_ref(),
                        SegmentationPrimitiveGeometry::Brush { centerline, .. } => {
                            centerline.as_ref()
                        }
                    };
                    for (point_index, candidate) in points.iter().copied().enumerate() {
                        update_handle_candidate(
                            &mut closest,
                            point,
                            candidate,
                            tolerance_squared,
                            id,
                            EditableHandle::SegmentPrimitivePoint {
                                primitive_index,
                                point_index,
                            },
                        );
                    }
                }
            } else if let Some(measurement) = self.document.measurement(id) {
                for (endpoint_index, candidate) in measurement.endpoints().into_iter().enumerate() {
                    update_handle_candidate(
                        &mut closest,
                        point,
                        candidate,
                        tolerance_squared,
                        id,
                        EditableHandle::MeasurementEndpoint { endpoint_index },
                    );
                }
            }
        }
        let Some((_, object_id, handle)) = closest else {
            return false;
        };
        self.handle_drag = Some(HandleDrag {
            object_id,
            handle,
            before: Arc::clone(&self.document),
        });
        true
    }

    pub(super) fn update_handle_drag(
        &mut self,
        point: Point2,
        physical_length_mm: impl FnOnce(Point2, Point2) -> Option<f64>,
    ) -> ViewerResult<()> {
        let Some(drag) = &self.handle_drag else {
            return Ok(());
        };
        let object_id = drag.object_id;
        let handle = drag.handle;
        let mut candidate = (*self.document).clone();
        match handle {
            EditableHandle::PointFinding => {
                candidate
                    .replace_vector_geometry(object_id, VectorFindingGeometry::Point(point))?;
            }
            EditableHandle::PolygonVertex {
                component_index,
                vertex_index,
            } => {
                candidate.move_vector_vertex(object_id, component_index, vertex_index, point)?;
            }
            EditableHandle::MeasurementEndpoint { endpoint_index } => {
                let mut endpoints = candidate
                    .measurement(object_id)
                    .ok_or_else(|| {
                        ViewerError::InvalidInput("the dragged measurement no longer exists".into())
                    })?
                    .endpoints();
                endpoints[endpoint_index] = point;
                let length = physical_length_mm(endpoints[0], endpoints[1]);
                candidate.set_measurement_endpoints(object_id, endpoints, length)?;
            }
            EditableHandle::SegmentPrimitivePoint {
                primitive_index,
                point_index,
            } => {
                candidate.move_segment_primitive_point(
                    object_id,
                    primitive_index,
                    point_index,
                    point,
                )?;
            }
        }
        candidate.validate()?;
        self.document = Arc::new(candidate);
        self.invalidate_spatial_index();
        Ok(())
    }

    pub(super) fn finish_handle_drag(&mut self) -> bool {
        let Some(drag) = self.handle_drag.take() else {
            return false;
        };
        if drag.before.revision() == self.document.revision() {
            return false;
        }
        self.history.record(
            "Move geometry handle",
            drag.before,
            Arc::clone(&self.document),
        );
        true
    }

    pub(super) fn cancel_handle_drag(&mut self) -> bool {
        let Some(drag) = self.handle_drag.take() else {
            return false;
        };
        self.document = drag.before;
        self.invalidate_spatial_index();
        true
    }

    #[must_use]
    pub(super) const fn handle_drag_active(&self) -> bool {
        self.handle_drag.is_some()
    }

    pub(super) fn reclassify_selection(&mut self, class_id: &str) -> ViewerResult<usize> {
        let ids = self.selection.iter().copied().collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(0);
        }
        self.ensure_objects_editable(&ids)?;
        self.edit("Reclassify selection", |document| {
            for id in &ids {
                document.reclassify_object(*id, class_id)?;
            }
            Ok(ids.len())
        })
    }

    pub(super) fn set_selected_name(&mut self, name: Option<&str>) -> ViewerResult<()> {
        let id = self.single_selection()?;
        self.ensure_objects_editable(&[id])?;
        self.edit("Rename finding", |document| {
            document.set_object_name(id, name)
        })
    }

    pub(super) fn set_selected_comment(&mut self, comment: Option<&str>) -> ViewerResult<()> {
        let id = self.single_selection()?;
        self.ensure_objects_editable(&[id])?;
        self.edit("Edit finding comment", |document| {
            document.set_object_comment(id, comment)
        })
    }

    pub(super) fn set_selected_finding_site(
        &mut self,
        site: Option<&dicom_viewer_core::DicomCode>,
    ) -> ViewerResult<()> {
        let id = self.single_selection()?;
        self.ensure_objects_editable(&[id])?;
        self.edit("Set finding site", |document| {
            document.set_object_finding_site(id, site)
        })
    }

    #[must_use]
    pub(super) fn active_class_id(&self) -> &str {
        match self.active_tool {
            ActiveTool::Point => &self.point_class_id,
            _ => &self.region_class_id,
        }
    }

    pub(super) fn set_active_class(&mut self, class_id: impl Into<String>) -> ViewerResult<()> {
        if self.draft.is_some() {
            return Err(ViewerError::InvalidInput(
                "finish, resume, or discard the current polygon before changing class".into(),
            ));
        }
        let class_id = class_id.into();
        let class = self.document.scheme().class(&class_id).ok_or_else(|| {
            ViewerError::InvalidInput("selected annotation class does not exist".into())
        })?;
        match class.geometry() {
            dicom_viewer_core::AnnotationClassGeometry::Region => self.region_class_id = class_id,
            dicom_viewer_core::AnnotationClassGeometry::Point => self.point_class_id = class_id,
        }
        self.draft_undo.clear();
        Ok(())
    }

    pub(super) fn migrate_scheme(
        &mut self,
        target: AnnotationScheme,
        mappings: &BTreeMap<String, String>,
    ) -> ViewerResult<()> {
        if self.draft.is_some() {
            return Err(ViewerError::InvalidInput(
                "finish, resume, or discard the current polygon before changing annotation scheme"
                    .into(),
            ));
        }
        self.edit("Migrate annotation scheme", |document| {
            document.migrate_scheme(target, mappings)
        })
    }

    #[must_use]
    pub(super) const fn segment_operation(&self) -> SegmentOperation {
        self.segment_operation
    }

    pub(super) fn set_segment_operation(&mut self, operation: SegmentOperation) {
        self.segment_operation = operation;
    }

    #[must_use]
    pub(super) const fn brush_diameter(&self) -> f64 {
        self.brush_diameter
    }

    pub(super) fn adjust_brush_diameter(&mut self, scale: f64) {
        self.brush_diameter = (self.brush_diameter * scale).clamp(1.0, 20_000.0);
    }

    #[must_use]
    #[cfg(test)]
    pub(super) const fn active_vector_layer(&self) -> Uuid {
        self.active_vector_layer
    }

    pub(super) fn ensure_segmentation_layer(&mut self) -> ViewerResult<Uuid> {
        if self.draft.is_some() {
            return Err(ViewerError::InvalidInput(
                "finish, resume, or discard the current polygon before activating Brush".into(),
            ));
        }
        if let Some(id) = self.active_segmentation_layer {
            self.editing_representation = EditingRepresentation::Segmentation;
            self.draft_undo.clear();
            return Ok(id);
        }
        let id = self.edit("Create segmentation layer", |document| {
            Ok(document.ensure_manual_segmentation_layer())
        })?;
        self.active_segmentation_layer = Some(id);
        self.editing_representation = EditingRepresentation::Segmentation;
        self.draft_undo.clear();
        Ok(id)
    }

    #[must_use]
    pub(super) const fn editing_representation(&self) -> EditingRepresentation {
        self.editing_representation
    }

    pub(super) fn use_vector_layer(&mut self, layer_id: Uuid) -> ViewerResult<()> {
        if self.draft.is_some() {
            return Err(ViewerError::InvalidInput(
                "finish, resume, or discard the current polygon before changing layers".into(),
            ));
        }
        if !self
            .document
            .vector_layers()
            .iter()
            .any(|layer| layer.id() == layer_id)
        {
            return Err(ViewerError::InvalidInput(
                "the selected vector layer does not exist".into(),
            ));
        }
        self.active_vector_layer = layer_id;
        self.editing_representation = EditingRepresentation::Vector;
        self.draft_undo.clear();
        Ok(())
    }

    pub(super) fn use_segmentation_layer(&mut self, layer_id: Uuid) -> ViewerResult<()> {
        if self.draft.is_some() {
            return Err(ViewerError::InvalidInput(
                "finish, resume, or discard the current polygon before changing layers".into(),
            ));
        }
        if !self
            .document
            .segmentation_layers()
            .iter()
            .any(|layer| layer.id() == layer_id)
        {
            return Err(ViewerError::InvalidInput(
                "the selected segmentation layer does not exist".into(),
            ));
        }
        self.active_segmentation_layer = Some(layer_id);
        self.editing_representation = EditingRepresentation::Segmentation;
        self.draft_undo.clear();
        Ok(())
    }

    pub(super) fn begin_new_segment(&mut self) {
        self.draft_undo.clear();
        self.clear_selection();
    }

    pub(super) fn add_external_annotation(
        &mut self,
        name: impl Into<String>,
        source_path: Option<PathBuf>,
        document: AnnotationDocument,
    ) -> ViewerResult<Uuid> {
        let count = document.groups().len() as u64;
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::DicomAnn,
            source_path,
            None,
            count,
        )?;
        self.external_payloads
            .insert(id, ExternalLayerPayload::Annotation(Arc::new(document)));
        Ok(id)
    }

    pub(super) fn add_external_pathology(
        &mut self,
        name: impl Into<String>,
        session: PathologySession,
    ) -> ViewerResult<Uuid> {
        let source_path = Some(session.geojson_path().to_path_buf());
        let source_digest = Some(session.semantic_digest().to_owned());
        let count = session.preview().features().len() as u64;
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::ProfiledGeoJson,
            source_path,
            source_digest,
            count,
        )?;
        self.external_payloads
            .insert(id, ExternalLayerPayload::ProfiledGeoJson(Arc::new(session)));
        Ok(id)
    }

    pub(super) fn add_external_segmentation(
        &mut self,
        name: impl Into<String>,
        source_path: Option<PathBuf>,
        document: SegmentationDocument,
    ) -> ViewerResult<(Uuid, Vec<InteroperabilityDiagnostic>)> {
        let count = document.segments().len() as u64;
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::DicomSeg,
            source_path,
            None,
            count,
        )?;
        let (vector_groups, diagnostics) =
            if document.kind() == dicom_viewer_core::SegmentationKind::Fractional {
                (None, Vec::new())
            } else {
                let projection =
                    document.vectorized_annotations(SegToAnnConversionPolicy::AllowLoss)?;
                let diagnostics = projection.diagnostics().to_vec();
                (Some(Arc::from(projection.into_groups())), diagnostics)
            };
        self.external_payloads.insert(
            id,
            ExternalLayerPayload::Segmentation {
                document: Arc::new(document),
                vector_groups,
            },
        );
        Ok((id, diagnostics))
    }

    pub(super) fn add_external_report(
        &mut self,
        name: impl Into<String>,
        source_path: Option<PathBuf>,
        session: ReportSession,
    ) -> ViewerResult<Uuid> {
        let count = session.document().groups().len() as u64;
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::DicomSr,
            source_path,
            None,
            count,
        )?;
        self.external_payloads
            .insert(id, ExternalLayerPayload::Report(Arc::new(session)));
        Ok(id)
    }

    pub(super) fn add_external_heatmap(
        &mut self,
        name: impl Into<String>,
        session: RasterSession,
        context: &eframe::egui::Context,
    ) -> ViewerResult<Uuid> {
        let source_path = Some(session.raster_path().to_path_buf());
        let source_digest = Some(session.semantic_digest().to_owned());
        let count = u64::from(session.frame_count())
            .saturating_mul(u64::try_from(session.selected_channel_count()).unwrap_or(u64::MAX));
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::Heatmap,
            source_path,
            source_digest,
            count,
        )?;
        let texture = session.load_texture(context);
        self.external_payloads.insert(
            id,
            ExternalLayerPayload::Heatmap {
                session: Arc::new(session),
                texture,
            },
        );
        Ok(id)
    }

    fn upsert_external_layer(
        &mut self,
        name: String,
        kind: ExternalLayerKind,
        source_path: Option<PathBuf>,
        source_digest: Option<String>,
        source_object_count: u64,
    ) -> ViewerResult<Uuid> {
        if let Some(id) = self.matching_external_layer(&kind, source_path.as_deref()) {
            self.edit("Load source layer", |workspace| {
                workspace.hydrate_external_layer(id, source_object_count, source_digest)
            })?;
            return Ok(id);
        }
        let reference = ExternalLayerReference::new(
            name,
            kind,
            source_path,
            source_digest,
            source_object_count,
        );
        let id = reference.id();
        self.edit("Import source layer", |document| {
            document.add_external_layer(reference)
        })?;
        Ok(id)
    }

    pub(super) fn ensure_discovered_external_stub(
        &mut self,
        name: impl Into<String>,
        kind: ExternalLayerKind,
        source_path: PathBuf,
    ) -> ViewerResult<Uuid> {
        if let Some(id) = self.matching_external_layer(&kind, Some(&source_path)) {
            return Ok(id);
        }
        let reference = ExternalLayerReference::new(name, kind, Some(source_path), None, 0);
        let id = reference.id();
        let mut candidate = (*self.document).clone();
        candidate.add_external_layer(reference)?;
        candidate.validate()?;
        self.document = Arc::new(candidate);
        self.invalidate_spatial_index();
        Ok(id)
    }

    pub(super) fn remove_external_layer(&mut self, layer_id: Uuid) -> ViewerResult<bool> {
        self.edit("Remove source layer", |document| {
            document.remove_external_layer(layer_id)
        })
    }

    #[must_use]
    pub(super) fn external_payload(&self, layer_id: Uuid) -> Option<&ExternalLayerPayload> {
        self.external_payloads.get(&layer_id)
    }

    fn matching_external_layer(
        &self,
        kind: &ExternalLayerKind,
        source_path: Option<&Path>,
    ) -> Option<Uuid> {
        let source_path = source_path?;
        self.document
            .external_layers()
            .iter()
            .find(|layer| layer.kind() == kind && layer.source_path() == Some(source_path))
            .map(ExternalLayerReference::id)
    }

    pub(super) fn external_classes(
        &self,
        layer_id: Uuid,
    ) -> ViewerResult<Vec<ExternalClassDescriptor>> {
        let objects = self.prepare_external_objects(layer_id)?;
        let mut classes = BTreeMap::<String, ExternalClassDescriptor>::new();
        for object in objects {
            let Some(geometry) = object.geometry else {
                continue;
            };
            let editable = object.promotion.is_some();
            let entry = classes.entry(object.class_key.clone()).or_insert_with(|| {
                let exact_scheme_class_id = self
                    .document
                    .scheme()
                    .classes()
                    .iter()
                    .find(|class| class.concept_key().as_str() == object.class_key)
                    .map(|class| class.id().to_owned());
                ExternalClassDescriptor {
                    key: object.class_key,
                    label: object.label,
                    geometry,
                    object_count: 0,
                    exact_scheme_class_id,
                    editable,
                }
            });
            entry.object_count = entry.object_count.saturating_add(1);
            entry.editable &= editable;
        }
        Ok(classes.into_values().collect())
    }

    pub(super) fn external_objects(
        &self,
        layer_id: Uuid,
    ) -> ViewerResult<Vec<ExternalObjectDescriptor>> {
        self.prepare_external_objects(layer_id).map(|objects| {
            objects
                .into_iter()
                .map(|object| ExternalObjectDescriptor {
                    promoted: self.source_object_was_promoted(layer_id, &object.source_object_id),
                    promotable: object.promotion.is_some(),
                    source_object_id: object.source_object_id,
                    label: object.label,
                    class_key: object.class_key,
                })
                .collect()
        })
    }

    pub(super) fn set_external_class_mapping(
        &mut self,
        layer_id: Uuid,
        source_class_key: &str,
        target_class_id: &str,
    ) -> ViewerResult<()> {
        let source = self
            .external_classes(layer_id)?
            .into_iter()
            .find(|class| class.key == source_class_key)
            .ok_or_else(|| {
                ViewerError::InvalidInput("the external source class does not exist".into())
            })?;
        let target = self
            .document
            .scheme()
            .class(target_class_id)
            .ok_or_else(|| {
                ViewerError::InvalidInput(
                    "the mapping target is not in the pinned annotation scheme".into(),
                )
            })?;
        if source.geometry != target.geometry() {
            return Err(ViewerError::InvalidInput(
                "external class mappings cannot change point/region geometry".into(),
            ));
        }
        let source_class_key = source_class_key.to_owned();
        let target_class_id = target_class_id.to_owned();
        self.edit("Map external class", move |document| {
            document.set_external_class_mapping(layer_id, &source_class_key, &target_class_id)
        })
    }

    pub(super) fn promote_external_object(
        &mut self,
        layer_id: Uuid,
        source_object_id: &str,
    ) -> ViewerResult<Uuid> {
        if self.source_object_was_promoted(layer_id, source_object_id) {
            return Err(ViewerError::InvalidInput(
                "the selected source object is already a tracked finding".into(),
            ));
        }
        let object = self
            .prepare_external_objects(layer_id)?
            .into_iter()
            .find(|object| object.source_object_id == source_object_id)
            .ok_or_else(|| {
                ViewerError::InvalidInput("the external source object does not exist".into())
            })?;
        let class_id = self.external_mapping_target(layer_id, &object.class_key)?;
        let promotion = object.promotion.ok_or_else(|| {
            ViewerError::Unsupported(
                "the selected external object cannot be represented by an editable workspace geometry".into(),
            )
        })?;
        let vector_layer = self.active_vector_layer;
        let source_object_id = object.source_object_id;
        let id = self.edit("Promote source object", move |document| {
            apply_external_promotion(
                document,
                vector_layer,
                layer_id,
                source_object_id,
                &class_id,
                promotion,
            )
        })?;
        if self.document.segment(id).is_some() {
            self.active_segmentation_layer = self
                .document
                .segmentation_layers()
                .iter()
                .find(|layer| {
                    layer
                        .segments()
                        .iter()
                        .any(|segment| segment.object_id() == id)
                })
                .map(|layer| layer.id());
        }
        self.select_only(id);
        Ok(id)
    }

    pub(super) fn make_external_layer_editable(
        &mut self,
        layer_id: Uuid,
    ) -> ViewerResult<Vec<Uuid>> {
        let objects = self.prepare_external_objects(layer_id)?;
        if objects.is_empty() {
            return Err(ViewerError::InvalidInput(
                "the external layer contains no convertible objects".into(),
            ));
        }
        if objects
            .iter()
            .any(|object| self.source_object_was_promoted(layer_id, &object.source_object_id))
        {
            return Err(ViewerError::InvalidInput(
                "the external layer already contains promoted objects; promote the remaining objects individually".into(),
            ));
        }
        let prepared = objects
            .into_iter()
            .map(|object| {
                let class_id = self.external_mapping_target(layer_id, &object.class_key)?;
                let promotion = object.promotion.ok_or_else(|| {
                    ViewerError::Unsupported(format!(
                        "source object {} cannot be converted losslessly",
                        object.source_object_id
                    ))
                })?;
                Ok((object.source_object_id, class_id, promotion))
            })
            .collect::<ViewerResult<Vec<_>>>()?;
        let vector_layer = self.active_vector_layer;
        let ids = self.edit("Make external layer editable", move |document| {
            prepared
                .into_iter()
                .map(|(source_object_id, class_id, promotion)| {
                    apply_external_promotion(
                        document,
                        vector_layer,
                        layer_id,
                        source_object_id,
                        &class_id,
                        promotion,
                    )
                })
                .collect::<ViewerResult<Vec<_>>>()
        })?;
        self.selection = ids.iter().copied().collect();
        if ids.iter().any(|id| self.document.segment(*id).is_some()) {
            self.active_segmentation_layer = self
                .document
                .segmentation_layers()
                .iter()
                .find(|layer| {
                    layer
                        .segments()
                        .iter()
                        .any(|segment| ids.contains(&segment.object_id()))
                })
                .map(|layer| layer.id());
        }
        Ok(ids)
    }

    fn external_mapping_target(
        &self,
        layer_id: Uuid,
        source_class_key: &str,
    ) -> ViewerResult<String> {
        self.document
            .external_layers()
            .iter()
            .find(|layer| layer.id() == layer_id)
            .and_then(|layer| layer.class_mappings().get(source_class_key))
            .cloned()
            .ok_or_else(|| {
                ViewerError::InvalidInput(
                    "every source class needs an explicit mapping before promotion".into(),
                )
            })
    }

    fn prepare_external_objects(
        &self,
        layer_id: Uuid,
    ) -> ViewerResult<Vec<PreparedExternalObject>> {
        if !self
            .document
            .external_layers()
            .iter()
            .any(|layer| layer.id() == layer_id)
        {
            return Err(ViewerError::InvalidInput(
                "the external source layer does not exist".into(),
            ));
        }
        match self.external_payloads.get(&layer_id) {
            Some(ExternalLayerPayload::Annotation(document)) => {
                prepare_annotation_objects(document)
            }
            Some(ExternalLayerPayload::ProfiledGeoJson(session)) => session
                .editable_ann()
                .map_or_else(|| Ok(Vec::new()), prepare_annotation_objects),
            Some(ExternalLayerPayload::Segmentation { document, .. }) => {
                prepare_segmentation_objects(document)
            }
            Some(ExternalLayerPayload::Report(session)) => {
                prepare_report_objects(session.document())
            }
            Some(ExternalLayerPayload::Heatmap { .. }) => Ok(Vec::new()),
            None => Ok(Vec::new()),
        }
    }

    fn source_object_was_promoted(&self, layer_id: Uuid, source_object_id: &str) -> bool {
        self.document
            .vector_findings()
            .map(|finding| finding.provenance())
            .chain(self.document.segments().map(|segment| segment.provenance()))
            .chain(
                self.document
                    .measurements()
                    .iter()
                    .map(|measurement| measurement.provenance()),
            )
            .any(|provenance| {
                matches!(
                    provenance,
                    WorkspaceObjectProvenance::Promoted {
                        source_layer_id,
                        source_object_id: promoted_id,
                    } if *source_layer_id == layer_id && promoted_id == source_object_id
                )
            })
    }

    pub(super) fn refresh_spatial_index(&mut self) -> ViewerResult<()> {
        if self.spatial_revision != self.document.revision() {
            self.spatial_index = WorkspaceSpatialIndex::build(&self.document)?;
            self.spatial_revision = self.document.revision();
        }
        Ok(())
    }

    #[must_use]
    pub(super) fn spatial_index(&self) -> &WorkspaceSpatialIndex {
        &self.spatial_index
    }

    #[must_use]
    pub(super) fn hit_test(&self, point: Point2, tolerance: f64) -> Option<Uuid> {
        let query = [
            point.x - tolerance,
            point.y - tolerance,
            point.x + tolerance,
            point.y + tolerance,
        ];
        self.spatial_index
            .query(query, 256)
            .into_iter()
            .filter_map(|id| {
                object_distance(self.document(), id, point).map(|distance| (id, distance))
            })
            .filter(|(_, distance)| *distance <= tolerance)
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(id, _)| id)
    }

    #[must_use]
    pub(super) fn object_bounds(&self, id: Uuid) -> Option<[f64; 4]> {
        self.spatial_index.bounds(id)
    }

    fn invalidate_spatial_index(&mut self) {
        self.spatial_revision = u64::MAX;
    }

    fn single_selection(&self) -> ViewerResult<Uuid> {
        if self.selection.len() != 1 {
            return Err(ViewerError::InvalidInput(
                "select exactly one tracked object".into(),
            ));
        }
        Ok(*self
            .selection
            .iter()
            .next()
            .expect("one selected object exists"))
    }

    fn ensure_layer_editable(&self, layer_id: Uuid) -> ViewerResult<()> {
        if self.document.presentation().layer(layer_id).locked {
            return Err(ViewerError::InvalidInput(
                "the active annotation layer is locked".into(),
            ));
        }
        Ok(())
    }

    fn ensure_objects_editable(&self, object_ids: &[Uuid]) -> ViewerResult<()> {
        if object_ids.iter().copied().any(|id| self.object_locked(id)) {
            return Err(ViewerError::InvalidInput(
                "the selection contains an object on a locked layer".into(),
            ));
        }
        Ok(())
    }

    fn object_locked(&self, object_id: Uuid) -> bool {
        self.document.vector_layers().iter().any(|layer| {
            layer
                .findings()
                .iter()
                .any(|finding| finding.object_id() == object_id)
                && self.document.presentation().layer(layer.id()).locked
        }) || self.document.segmentation_layers().iter().any(|layer| {
            layer
                .segments()
                .iter()
                .any(|segment| segment.object_id() == object_id)
                && self.document.presentation().layer(layer.id()).locked
        })
    }
}

fn annotation_group_geometry(
    group: &dicom_viewer_core::AnnotationGroup,
) -> Option<AnnotationClassGeometry> {
    match group.geometry() {
        AnnotationGeometry::Points(_) => Some(AnnotationClassGeometry::Point),
        AnnotationGeometry::Polygons(_) => Some(AnnotationClassGeometry::Region),
        AnnotationGeometry::ReadOnly { graphic_type, .. } => match graphic_type {
            dicom_viewer_core::AnnotationGraphicType::Point => Some(AnnotationClassGeometry::Point),
            dicom_viewer_core::AnnotationGraphicType::Polygon => {
                Some(AnnotationClassGeometry::Region)
            }
            dicom_viewer_core::AnnotationGraphicType::Polyline
            | dicom_viewer_core::AnnotationGraphicType::Ellipse
            | dicom_viewer_core::AnnotationGraphicType::Rectangle => None,
        },
    }
}

fn prepare_annotation_objects(
    document: &AnnotationDocument,
) -> ViewerResult<Vec<PreparedExternalObject>> {
    let mut objects = Vec::new();
    for group in document.groups() {
        let Some(geometry) = annotation_group_geometry(group) else {
            continue;
        };
        let class_key = annotation_class_concept_key(
            geometry,
            group.category(),
            group.property_type(),
            group.property_type_modifiers(),
        )
        .to_string();
        let source_frame = SourceFrameContext::new(
            (group.referenced_optical_paths().len() == 1)
                .then(|| group.referenced_optical_paths()[0].clone()),
            None,
            None,
            None,
        );
        match group.geometry() {
            AnnotationGeometry::Points(points) => {
                for (index, point) in points.iter().copied().enumerate() {
                    let point = document.canonical_level0_pixel(
                        document.source(),
                        point.x,
                        point.y,
                        None,
                    )?;
                    objects.push(PreparedExternalObject {
                        source_object_id: format!("{}:{}", group.uid(), index + 1),
                        label: group.label().to_owned(),
                        class_key: class_key.clone(),
                        geometry: Some(geometry),
                        promotion: Some(PreparedExternalPromotion::Vector {
                            geometry: VectorFindingGeometry::Point(point),
                            tracking: None,
                            source_frame: source_frame.clone(),
                        }),
                    });
                }
            }
            AnnotationGeometry::Polygons(polygons) => {
                for (index, polygon) in polygons.iter().enumerate() {
                    let polygon = polygon
                        .iter()
                        .map(|point| {
                            document
                                .canonical_level0_pixel(document.source(), point.x, point.y, None)
                                .map_err(dicom_viewer_core::ViewerError::from)
                        })
                        .collect::<ViewerResult<Vec<_>>>()?;
                    objects.push(PreparedExternalObject {
                        source_object_id: format!("{}:{}", group.uid(), index + 1),
                        label: group.label().to_owned(),
                        class_key: class_key.clone(),
                        geometry: Some(geometry),
                        promotion: Some(PreparedExternalPromotion::Vector {
                            geometry: VectorFindingGeometry::regions(vec![polygon]),
                            tracking: None,
                            source_frame: source_frame.clone(),
                        }),
                    });
                }
            }
            AnnotationGeometry::ReadOnly { .. } => {
                for index in 0..group.annotation_count() {
                    objects.push(PreparedExternalObject {
                        source_object_id: format!("{}:{}", group.uid(), index + 1),
                        label: group.label().to_owned(),
                        class_key: class_key.clone(),
                        geometry: Some(geometry),
                        promotion: None,
                    });
                }
            }
        }
    }
    Ok(objects)
}

fn prepare_segmentation_objects(
    document: &SegmentationDocument,
) -> ViewerResult<Vec<PreparedExternalObject>> {
    let mut polygons = BTreeMap::<u16, Vec<Vec<Point2>>>::new();
    if document.editable() {
        for run in document.binary_runs()? {
            let x0 = f64::from(run.column_start());
            let x1 = f64::from(run.column_start().saturating_add(run.length()));
            let y0 = f64::from(run.row());
            let y1 = y0 + 1.0;
            polygons.entry(run.segment_number()).or_default().push(vec![
                Point2::new(x0, y0),
                Point2::new(x1, y0),
                Point2::new(x1, y1),
                Point2::new(x0, y1),
            ]);
        }
    }
    Ok(document
        .segments()
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let number = segment
                .source_segment_number()
                .unwrap_or_else(|| u16::try_from(index + 1).unwrap_or(u16::MAX));
            let class_key = annotation_class_concept_key(
                AnnotationClassGeometry::Region,
                segment.category(),
                segment.property_type(),
                segment.property_type_modifiers(),
            )
            .to_string();
            let tracking = segment
                .tracking_id()
                .zip(segment.tracking_uid())
                .and_then(|(id, uid)| TrackingIdentity::new(id, uid).ok());
            let primitives = polygons.remove(&number).map(|polygons| {
                polygons
                    .into_iter()
                    .map(|polygon| SegmentationPrimitive::polygon(SegmentOperation::Add, polygon))
                    .collect::<Vec<_>>()
            });
            PreparedExternalObject {
                source_object_id: format!("segment:{number}"),
                label: segment.label().to_owned(),
                class_key,
                geometry: Some(AnnotationClassGeometry::Region),
                promotion: primitives.filter(|primitives| !primitives.is_empty()).map(
                    |primitives| PreparedExternalPromotion::Segment {
                        primitives,
                        tracking,
                        source_frame: SourceFrameContext::default(),
                    },
                ),
            }
        })
        .collect())
}

fn prepare_report_objects(
    document: &StructuredReportDocument,
) -> ViewerResult<Vec<PreparedExternalObject>> {
    let mut objects = Vec::new();
    for group in document.groups() {
        let class_key = annotation_class_concept_key(
            AnnotationClassGeometry::Region,
            group.finding_category(),
            group.finding_type(),
            &[],
        )
        .to_string();
        let source_tracking = TrackingIdentity::new(group.tracking_id(), group.tracking_uid()).ok();
        for (measurement_index, measurement) in group.measurements().iter().enumerate() {
            for (coordinate_index, coordinates) in measurement.coordinates().iter().enumerate() {
                if coordinates.graphic() != dicom_viewer_core::CoordinateGraphic::Polyline
                    || coordinates.points().len() != 2
                {
                    continue;
                }
                let source_object_id = format!(
                    "{}:{}:{}",
                    group.tracking_uid(),
                    measurement_index + 1,
                    coordinate_index + 1
                );
                let physical_length_mm = measurement_length_mm(measurement);
                let endpoints = coordinates
                    .points()
                    .iter()
                    .map(|point| {
                        document
                            .source()
                            .slide_coordinate_to_pixel3(point.x, point.y, point.z)
                            .map_err(dicom_viewer_core::ViewerError::from)
                    })
                    .collect::<ViewerResult<Vec<_>>>()?;
                let promotion = physical_length_mm.map(|physical_length_mm| {
                    PreparedExternalPromotion::Measurement {
                        endpoints: [endpoints[0], endpoints[1]],
                        physical_length_mm,
                        tracking: source_tracking.clone(),
                        source_frame: SourceFrameContext::default(),
                    }
                });
                objects.push(PreparedExternalObject {
                    source_object_id,
                    label: group.finding_type().meaning().to_owned(),
                    class_key: class_key.clone(),
                    geometry: Some(AnnotationClassGeometry::Region),
                    promotion,
                });
            }
        }
    }
    Ok(objects)
}

fn measurement_length_mm(
    measurement: &dicom_viewer_core::StructuredReportMeasurement,
) -> Option<f64> {
    if measurement.concept().scheme() != "SCT"
        || measurement.concept().value() != "410668003"
        || measurement.unit().scheme() != "UCUM"
        || !measurement.value().is_finite()
        || measurement.value() <= 0.0
    {
        return None;
    }
    match measurement.unit().value() {
        "mm" => Some(measurement.value()),
        "um" | "µm" => Some(measurement.value() / 1_000.0),
        _ => None,
    }
}

fn object_distance(document: &WorkspaceDocument, id: Uuid, point: Point2) -> Option<f64> {
    if let Some(finding) = document.finding(id) {
        return Some(match finding.geometry() {
            dicom_viewer_core::VectorFindingGeometry::Point(candidate) => {
                (candidate.x - point.x).hypot(candidate.y - point.y)
            }
            dicom_viewer_core::VectorFindingGeometry::Regions(components) => components
                .iter()
                .map(|component| polygon_distance(component, point))
                .fold(f64::INFINITY, f64::min),
        });
    }
    if let Some(segment) = document.segment(id) {
        let geometry = document.composite_segment(segment.object_id()).ok()?;
        return geometry
            .components()
            .iter()
            .map(|component| {
                let inside = dicom_viewer_core::polygon_contains_point(component.exterior(), point)
                    && !component
                        .holes()
                        .iter()
                        .any(|hole| dicom_viewer_core::polygon_contains_point(hole, point));
                if inside {
                    0.0
                } else {
                    std::iter::once(component.exterior())
                        .chain(component.holes().iter().map(Vec::as_slice))
                        .map(|ring| ring_edge_distance(ring, point))
                        .fold(f64::INFINITY, f64::min)
                }
            })
            .min_by(f64::total_cmp);
    }
    let measurement = document.measurement(id)?;
    let endpoints = measurement.endpoints();
    Some(segment_distance(endpoints[0], endpoints[1], point))
}

fn update_handle_candidate(
    closest: &mut Option<(f64, Uuid, EditableHandle)>,
    target: Point2,
    candidate: Point2,
    tolerance_squared: f64,
    object_id: Uuid,
    handle: EditableHandle,
) {
    let dx = candidate.x - target.x;
    let dy = candidate.y - target.y;
    let distance_squared = dx * dx + dy * dy;
    if distance_squared > tolerance_squared
        || closest
            .as_ref()
            .is_some_and(|(best, _, _)| *best <= distance_squared)
    {
        return;
    }
    *closest = Some((distance_squared, object_id, handle));
}

fn polygon_distance(polygon: &[Point2], point: Point2) -> f64 {
    if dicom_viewer_core::polygon_contains_point(polygon, point) {
        0.0
    } else {
        ring_edge_distance(polygon, point)
    }
}

fn ring_edge_distance(points: &[Point2], point: Point2) -> f64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(start, end)| segment_distance(*start, *end, point))
        .fold(f64::INFINITY, f64::min)
}

fn segment_distance(start: Point2, end: Point2, point: Point2) -> f64 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_squared = dx * dx + dy * dy;
    if length_squared <= f64::EPSILON {
        return (point.x - start.x).hypot(point.y - start.y);
    }
    let fraction =
        (((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared).clamp(0.0, 1.0);
    let closest = Point2::new(start.x + fraction * dx, start.y + fraction * dy);
    (point.x - closest.x).hypot(point.y - closest.y)
}

#[cfg(test)]
mod persistence_tests;
#[cfg(test)]
mod tests;
