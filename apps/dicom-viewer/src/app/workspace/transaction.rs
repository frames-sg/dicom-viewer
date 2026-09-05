use super::*;

impl WorkspaceRuntime {
    pub(in crate::app) fn new(
        source_identity: ViewerSourceIdentity,
        scheme: AnnotationScheme,
    ) -> ViewerResult<Self> {
        Self::from_document(WorkspaceDocument::new(source_identity, scheme)?)
    }

    #[cfg(test)]
    pub(in crate::app) fn with_history_limits(
        source_identity: ViewerSourceIdentity,
        scheme: AnnotationScheme,
        max_commands: usize,
        max_retained_bytes: usize,
    ) -> ViewerResult<Self> {
        let mut runtime = Self::new(source_identity, scheme)?;
        runtime.history = WorkspaceHistory::with_limits(max_commands, max_retained_bytes);
        Ok(runtime)
    }

    pub(in crate::app) fn from_document(document: WorkspaceDocument) -> ViewerResult<Self> {
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
            active_tool: ActiveTool::Pan,
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
    pub(in crate::app) fn document(&self) -> &WorkspaceDocument {
        &self.document
    }

    #[must_use]
    pub(in crate::app) fn document_snapshot(&self) -> Arc<WorkspaceDocument> {
        Arc::clone(&self.document)
    }

    pub(in crate::app) fn edit<T>(
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

    pub(in crate::app) fn undo(&mut self) -> bool {
        let Some(document) = self.history.undo() else {
            return false;
        };
        self.document = document;
        self.selection
            .retain(|id| self.document.object(*id).is_some());
        self.invalidate_spatial_index();
        true
    }

    pub(in crate::app) fn redo(&mut self) -> bool {
        let Some(document) = self.history.redo() else {
            return false;
        };
        self.document = document;
        self.invalidate_spatial_index();
        true
    }

    #[must_use]
    pub(in crate::app) fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    #[must_use]
    pub(in crate::app) fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    #[must_use]
    pub(in crate::app) fn history_truncated(&self) -> bool {
        self.history.truncated()
    }

    #[must_use]
    pub(in crate::app) fn undo_label(&self) -> Option<&str> {
        self.history.undo_label()
    }

    #[must_use]
    pub(in crate::app) fn redo_label(&self) -> Option<&str> {
        self.history.redo_label()
    }
}
