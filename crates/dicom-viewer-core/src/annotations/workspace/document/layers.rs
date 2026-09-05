use uuid::Uuid;

use crate::{Result, ViewerError};

use super::super::model::{ExternalLayerReference, LayerPresentation};
use super::WorkspaceDocument;

impl WorkspaceDocument {
    pub fn add_external_layer(&mut self, layer: ExternalLayerReference) -> Result<Uuid> {
        if layer.name().trim().is_empty() || layer.name().len() > 256 {
            return Err(ViewerError::InvalidInput(
                "external layer name must be 1..=256 bytes".into(),
            ));
        }
        if self
            .external_layers
            .iter()
            .any(|item| item.id() == layer.id())
        {
            return Err(ViewerError::InvalidInput(
                "external layer ID already exists".into(),
            ));
        }
        let id = layer.id();
        self.presentation.insert_layer(id);
        self.external_layers.push(layer);
        self.bump_revision();
        Ok(id)
    }

    pub fn remove_external_layer(&mut self, layer_id: Uuid) -> Result<bool> {
        let Some(index) = self
            .external_layers
            .iter()
            .position(|layer| layer.id() == layer_id)
        else {
            return Ok(false);
        };
        self.external_layers.remove(index);
        self.presentation.remove_layer(layer_id);
        self.bump_revision();
        Ok(true)
    }

    /// Updates an unloaded external-layer reference after its payload has been validated.
    pub fn hydrate_external_layer(
        &mut self,
        layer_id: Uuid,
        source_object_count: u64,
        source_digest: Option<String>,
    ) -> Result<()> {
        if source_digest
            .as_deref()
            .is_some_and(|digest| digest.is_empty() || digest.len() > 128)
        {
            return Err(ViewerError::InvalidInput(
                "external source digest must be 1..=128 bytes when present".into(),
            ));
        }
        let layer = self
            .external_layers
            .iter_mut()
            .find(|layer| layer.id() == layer_id)
            .ok_or_else(|| {
                ViewerError::InvalidInput("the external source layer does not exist".into())
            })?;
        if let (Some(expected), Some(actual)) = (layer.source_digest(), source_digest.as_deref()) {
            if expected != actual {
                return Err(ViewerError::InvalidInput(
                    "the external source content changed; remove the saved source layer and import it explicitly"
                        .into(),
                ));
            }
        }
        let source_digest = source_digest.or_else(|| layer.source_digest().map(ToOwned::to_owned));
        layer.hydrate(source_object_count, source_digest);
        self.bump_revision();
        Ok(())
    }

    pub fn set_external_class_mapping(
        &mut self,
        layer_id: Uuid,
        source_class: &str,
        target_class_id: &str,
    ) -> Result<()> {
        if source_class.trim().is_empty() || source_class.len() > 1_024 {
            return Err(ViewerError::InvalidInput(
                "external source class key must be 1..=1024 bytes".into(),
            ));
        }
        if self.scheme.class(target_class_id).is_none() {
            return Err(ViewerError::InvalidInput(
                "external class mapping target is not in the pinned annotation scheme".into(),
            ));
        }
        let layer = self
            .external_layers
            .iter_mut()
            .find(|layer| layer.id() == layer_id)
            .ok_or_else(|| {
                ViewerError::InvalidInput("the external source layer does not exist".into())
            })?;
        layer.set_class_mapping(source_class.to_owned(), target_class_id.to_owned());
        self.bump_revision();
        Ok(())
    }

    pub fn set_layer_presentation(
        &mut self,
        layer_id: Uuid,
        presentation: LayerPresentation,
    ) -> Result<()> {
        if !presentation.opacity.is_finite() || !(0.0..=1.0).contains(&presentation.opacity) {
            return Err(ViewerError::InvalidInput(
                "layer opacity must be between zero and one".into(),
            ));
        }
        if !self.layer_exists(layer_id) {
            return Err(ViewerError::InvalidInput(
                "the selected layer does not exist".into(),
            ));
        }
        self.presentation.set_layer(layer_id, presentation);
        self.bump_revision();
        Ok(())
    }

    pub fn set_object_visible(&mut self, object_id: Uuid, visible: bool) -> Result<()> {
        if !self.object_exists(object_id) {
            return Err(ViewerError::InvalidInput(
                "the selected workspace object does not exist".into(),
            ));
        }
        self.presentation.set_object_visible(object_id, visible);
        self.bump_revision();
        Ok(())
    }

    fn layer_exists(&self, id: Uuid) -> bool {
        self.vector_layers.iter().any(|layer| layer.id() == id)
            || self
                .segmentation_layers
                .iter()
                .any(|layer| layer.id() == id)
            || self.external_layers.iter().any(|layer| layer.id() == id)
    }
}
