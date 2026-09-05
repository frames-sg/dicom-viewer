use super::*;

impl WorkspaceRuntime {
    #[must_use]
    pub(in crate::app) const fn active_tool(&self) -> ActiveTool {
        self.active_tool
    }

    #[cfg(test)]
    pub(in crate::app) fn set_active_tool(&mut self, tool: ActiveTool) -> ViewerResult<()> {
        match self.request_tool(tool) {
            ToolTransitionOutcome::Applied => Ok(()),
            ToolTransitionOutcome::BlockedByDraft => Err(ViewerError::InvalidInput(
                "finish, resume, or discard the current polygon before switching tools".into(),
            )),
        }
    }

    pub(in crate::app) fn request_tool(&mut self, tool: ActiveTool) -> ToolTransitionOutcome {
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

    pub(in crate::app) fn resolve_draft_transition(
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

    pub(in crate::app) fn finish_draft(&mut self) -> ViewerResult<Uuid> {
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
    pub(in crate::app) fn draft(&self) -> Option<&DraftInteraction> {
        self.draft.as_ref()
    }

    pub(in crate::app) fn set_draft(&mut self, draft: DraftInteraction) {
        self.draft = Some(draft);
        self.draft_undo.clear();
    }

    pub(in crate::app) fn add_polygon_point(&mut self, point: Point2) -> ViewerResult<()> {
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

    pub(in crate::app) fn add_point_finding(&mut self, point: Point2) -> ViewerResult<Uuid> {
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

    pub(in crate::app) fn begin_brush_stroke_with_operation(
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

    pub(in crate::app) fn extend_brush_stroke(&mut self, point: Point2) {
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

    pub(in crate::app) fn finish_brush_stroke(&mut self) -> ViewerResult<Option<Uuid>> {
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

    pub(in crate::app) fn cancel_pointer_interaction(&mut self) {
        self.brush_stroke = None;
        self.brush_operation = None;
    }

    #[must_use]
    pub(in crate::app) fn brush_stroke(&self) -> Option<&[Point2]> {
        self.brush_stroke.as_deref()
    }

    pub(in crate::app) fn place_ruler_point(
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
    pub(in crate::app) const fn ruler_start(&self) -> Option<Point2> {
        self.ruler_start
    }

    pub(in crate::app) fn cancel_ruler(&mut self) -> bool {
        self.ruler_start.take().is_some()
    }

    pub(in crate::app) fn cancel_draft_step(&mut self) -> bool {
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

    pub(in crate::app) fn undo_draft_cancel(&mut self) -> bool {
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

    pub(in crate::app) fn discard_draft(&mut self) {
        self.draft = None;
        self.draft_undo.clear();
        self.pending_tool = None;
    }

    #[must_use]
    pub(in crate::app) fn active_class_id(&self) -> &str {
        match self.active_tool {
            ActiveTool::Point => &self.point_class_id,
            _ => &self.region_class_id,
        }
    }

    pub(in crate::app) fn set_active_class(
        &mut self,
        class_id: impl Into<String>,
    ) -> ViewerResult<()> {
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

    pub(in crate::app) fn migrate_scheme(
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
    pub(in crate::app) const fn segment_operation(&self) -> SegmentOperation {
        self.segment_operation
    }

    pub(in crate::app) fn set_segment_operation(&mut self, operation: SegmentOperation) {
        self.segment_operation = operation;
    }

    #[must_use]
    pub(in crate::app) const fn brush_diameter(&self) -> f64 {
        self.brush_diameter
    }

    pub(in crate::app) fn adjust_brush_diameter(&mut self, scale: f64) {
        self.brush_diameter = (self.brush_diameter * scale).clamp(1.0, 20_000.0);
    }

    #[must_use]
    #[cfg(test)]
    pub(in crate::app) const fn active_vector_layer(&self) -> Uuid {
        self.active_vector_layer
    }

    pub(in crate::app) fn ensure_segmentation_layer(&mut self) -> ViewerResult<Uuid> {
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
    pub(in crate::app) const fn editing_representation(&self) -> EditingRepresentation {
        self.editing_representation
    }

    pub(in crate::app) fn use_vector_layer(&mut self, layer_id: Uuid) -> ViewerResult<()> {
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

    pub(in crate::app) fn use_segmentation_layer(&mut self, layer_id: Uuid) -> ViewerResult<()> {
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

    pub(in crate::app) fn begin_new_segment(&mut self) {
        self.draft_undo.clear();
        self.clear_selection();
    }
}
