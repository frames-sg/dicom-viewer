use super::*;

impl WorkspaceRuntime {
    #[must_use]
    pub(in crate::app) fn selection(&self) -> &HashSet<Uuid> {
        &self.selection
    }

    pub(in crate::app) fn select_only(&mut self, object_id: Uuid) {
        self.selection.clear();
        self.selection.insert(object_id);
    }

    pub(in crate::app) fn toggle_selection(&mut self, object_id: Uuid) {
        if !self.selection.remove(&object_id) {
            self.selection.insert(object_id);
        }
    }

    pub(in crate::app) fn clear_selection(&mut self) {
        self.selection.clear();
    }

    pub(in crate::app) fn selected_segment(&self) -> Option<Uuid> {
        self.selection.iter().copied().find(|id| {
            self.document.object(*id).is_some_and(|object| {
                object.geometry_kind()
                    == dicom_viewer_core::WorkspaceObjectGeometryKind::Segmentation
            })
        })
    }

    pub(in crate::app) fn delete_selection(&mut self) -> ViewerResult<usize> {
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

    pub(in crate::app) fn reclassify_selection(&mut self, class_id: &str) -> ViewerResult<usize> {
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

    pub(in crate::app) fn set_selected_name(&mut self, name: Option<&str>) -> ViewerResult<()> {
        let id = self.single_selection()?;
        self.ensure_objects_editable(&[id])?;
        self.edit("Rename finding", |document| {
            document.set_object_name(id, name)
        })
    }

    pub(in crate::app) fn set_selected_comment(
        &mut self,
        comment: Option<&str>,
    ) -> ViewerResult<()> {
        let id = self.single_selection()?;
        self.ensure_objects_editable(&[id])?;
        self.edit("Edit finding comment", |document| {
            document.set_object_comment(id, comment)
        })
    }

    pub(in crate::app) fn set_selected_finding_site(
        &mut self,
        site: Option<&dicom_viewer_core::DicomCode>,
    ) -> ViewerResult<()> {
        let id = self.single_selection()?;
        self.ensure_objects_editable(&[id])?;
        self.edit("Set finding site", |document| {
            document.set_object_finding_site(id, site)
        })
    }

    pub(in crate::app) fn single_selection(&self) -> ViewerResult<Uuid> {
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

    pub(in crate::app) fn ensure_layer_editable(&self, layer_id: Uuid) -> ViewerResult<()> {
        if self.document.presentation().layer(layer_id).locked {
            return Err(ViewerError::InvalidInput(
                "the active annotation layer is locked".into(),
            ));
        }
        Ok(())
    }

    pub(in crate::app) fn ensure_objects_editable(&self, object_ids: &[Uuid]) -> ViewerResult<()> {
        if object_ids.iter().copied().any(|id| self.object_locked(id)) {
            return Err(ViewerError::InvalidInput(
                "the selection contains an object on a locked layer".into(),
            ));
        }
        Ok(())
    }

    pub(in crate::app) fn object_locked(&self, object_id: Uuid) -> bool {
        self.document
            .object_layer_id(object_id)
            .ok()
            .flatten()
            .is_some_and(|layer_id| self.document.presentation().layer(layer_id).locked)
    }
}
