use std::collections::HashSet;

use uuid::Uuid;

use crate::{AnnotationClassGeometry, Result, TrackingIdentity, ViewerError};

use super::super::composition::compose_segment;
use super::super::model::{
    ExternalLayerReference, SegmentOperation, SegmentationLayer, SegmentationPrimitive,
    VectorFindingGeometry, VectorLayer,
};
use super::{
    validate_measurement, validate_source_identity, validate_vector_geometry, WorkspaceDocument,
    MAX_COORDINATE_POINTS, MAX_EDITABLE_OBJECTS, WORKSPACE_SCHEMA_VERSION,
};

impl WorkspaceDocument {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != WORKSPACE_SCHEMA_VERSION {
            return Err(ViewerError::Unsupported(format!(
                "workspace schema version {} is not supported",
                self.schema_version
            )));
        }
        validate_source_identity(&self.source_identity)?;
        if self.object_count() > MAX_EDITABLE_OBJECTS {
            return Err(ViewerError::InvalidInput(format!(
                "workspace exceeds the {MAX_EDITABLE_OBJECTS} editable-object limit"
            )));
        }
        if self.coordinate_count() > MAX_COORDINATE_POINTS {
            return Err(ViewerError::InvalidInput(format!(
                "workspace exceeds the {MAX_COORDINATE_POINTS} coordinate-point limit"
            )));
        }

        let mut layer_ids = HashSet::new();
        for id in self
            .vector_layers
            .iter()
            .map(VectorLayer::id)
            .chain(self.segmentation_layers.iter().map(SegmentationLayer::id))
            .chain(self.external_layers.iter().map(ExternalLayerReference::id))
        {
            if !layer_ids.insert(id) {
                return Err(ViewerError::InvalidInput(
                    "workspace contains duplicate layer IDs".into(),
                ));
            }
        }
        for layer in &self.external_layers {
            if layer.name().trim().is_empty() || layer.name().len() > 256 {
                return Err(ViewerError::InvalidInput(
                    "external layer name must be 1..=256 bytes".into(),
                ));
            }
            for (source, target) in layer.class_mappings() {
                if source.trim().is_empty()
                    || source.len() > 1_024
                    || self.scheme.class(target).is_none()
                {
                    return Err(ViewerError::InvalidInput(
                        "external layer contains an invalid class mapping".into(),
                    ));
                }
            }
        }

        let mut object_ids = HashSet::new();
        let mut ordinals = HashSet::new();
        let mut tracking_ids = HashSet::new();
        let mut tracking_uids = HashSet::new();
        let mut max_ordinal = 0;
        for finding in self.vector_findings() {
            self.validate_class(
                finding.class_id(),
                match finding.geometry() {
                    VectorFindingGeometry::Point(_) => AnnotationClassGeometry::Point,
                    VectorFindingGeometry::Regions(_) => AnnotationClassGeometry::Region,
                },
            )?;
            validate_vector_geometry(finding.geometry(), self.source_identity.dimensions())?;
            validate_identity(
                finding.object_id(),
                finding.ordinal(),
                finding.tracking(),
                &mut object_ids,
                &mut ordinals,
                &mut tracking_ids,
                &mut tracking_uids,
            )?;
            max_ordinal = max_ordinal.max(finding.ordinal());
        }
        for segment in self.segments() {
            self.validate_class(segment.class_id(), AnnotationClassGeometry::Region)?;
            if segment
                .primitives()
                .first()
                .map(SegmentationPrimitive::operation)
                != Some(SegmentOperation::Add)
            {
                return Err(ViewerError::InvalidInput(
                    "a segmentation segment must start with an Add primitive".into(),
                ));
            }
            compose_segment(segment.primitives(), self.source_identity.dimensions())?;
            validate_identity(
                segment.object_id(),
                segment.ordinal(),
                segment.tracking(),
                &mut object_ids,
                &mut ordinals,
                &mut tracking_ids,
                &mut tracking_uids,
            )?;
            max_ordinal = max_ordinal.max(segment.ordinal());
        }
        for measurement in &self.measurements {
            self.validate_class(measurement.class_id(), AnnotationClassGeometry::Region)?;
            validate_measurement(
                measurement.endpoints(),
                measurement.physical_length_mm(),
                self.source_identity.dimensions(),
            )?;
            validate_identity(
                measurement.object_id(),
                measurement.ordinal(),
                measurement.tracking(),
                &mut object_ids,
                &mut ordinals,
                &mut tracking_ids,
                &mut tracking_uids,
            )?;
            max_ordinal = max_ordinal.max(measurement.ordinal());
        }
        if self.next_ordinal == 0 || self.next_ordinal <= max_ordinal {
            return Err(ViewerError::InvalidInput(
                "workspace next ordinal does not exceed every assigned ordinal".into(),
            ));
        }
        self.validate_sites_for_scheme(&self.scheme)?;
        self.presentation.validate(&layer_ids, &object_ids)?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_identity(
    object_id: Uuid,
    ordinal: u64,
    tracking: &TrackingIdentity,
    object_ids: &mut HashSet<Uuid>,
    ordinals: &mut HashSet<u64>,
    tracking_ids: &mut HashSet<String>,
    tracking_uids: &mut HashSet<String>,
) -> Result<()> {
    if object_id.is_nil() || !object_ids.insert(object_id) {
        return Err(ViewerError::InvalidInput(
            "workspace contains a nil or duplicate object ID".into(),
        ));
    }
    if ordinal == 0 || !ordinals.insert(ordinal) {
        return Err(ViewerError::InvalidInput(
            "workspace contains a zero or duplicate ordinal".into(),
        ));
    }
    TrackingIdentity::new(tracking.id(), tracking.uid())?;
    if !tracking_ids.insert(tracking.id().to_owned())
        || !tracking_uids.insert(tracking.uid().to_owned())
    {
        return Err(ViewerError::InvalidInput(
            "workspace contains a conflicting tracking identity".into(),
        ));
    }
    Ok(())
}
