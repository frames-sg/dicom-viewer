use uuid::Uuid;

use crate::{Result, TrackingIdentity, ViewerError};

use super::super::model::{
    ControlledFindingSite, SegmentationSegmentFinding, SourceFrameContext, VectorFinding,
    VectorFindingGeometry, WorkspaceLinearMeasurement, WorkspaceObjectProvenance,
};
use super::WorkspaceDocument;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceObjectGeometryKind {
    Point,
    Region,
    Segmentation,
    Measurement,
}

#[derive(Debug, Clone, Copy)]
pub enum WorkspaceObjectRef<'a> {
    Vector(&'a VectorFinding),
    Segment(&'a SegmentationSegmentFinding),
    Measurement(&'a WorkspaceLinearMeasurement),
}

impl<'a> WorkspaceObjectRef<'a> {
    #[must_use]
    pub fn object_id(self) -> Uuid {
        match self {
            Self::Vector(object) => object.object_id(),
            Self::Segment(object) => object.object_id(),
            Self::Measurement(object) => object.object_id(),
        }
    }

    #[must_use]
    pub fn ordinal(self) -> u64 {
        match self {
            Self::Vector(object) => object.ordinal(),
            Self::Segment(object) => object.ordinal(),
            Self::Measurement(object) => object.ordinal(),
        }
    }

    #[must_use]
    pub fn tracking(self) -> &'a TrackingIdentity {
        match self {
            Self::Vector(object) => object.tracking(),
            Self::Segment(object) => object.tracking(),
            Self::Measurement(object) => object.tracking(),
        }
    }

    #[must_use]
    pub fn class_id(self) -> &'a str {
        match self {
            Self::Vector(object) => object.class_id(),
            Self::Segment(object) => object.class_id(),
            Self::Measurement(object) => object.class_id(),
        }
    }

    #[must_use]
    pub fn finding_site(self) -> Option<&'a ControlledFindingSite> {
        match self {
            Self::Vector(object) => object.finding_site(),
            Self::Segment(object) => object.finding_site(),
            Self::Measurement(object) => object.finding_site(),
        }
    }

    #[must_use]
    pub fn name(self) -> Option<&'a str> {
        match self {
            Self::Vector(object) => object.name(),
            Self::Segment(object) => object.name(),
            Self::Measurement(object) => object.name(),
        }
    }

    #[must_use]
    pub fn comment(self) -> Option<&'a str> {
        match self {
            Self::Vector(object) => object.comment(),
            Self::Segment(object) => object.comment(),
            Self::Measurement(object) => object.comment(),
        }
    }

    #[must_use]
    pub fn provenance(self) -> &'a WorkspaceObjectProvenance {
        match self {
            Self::Vector(object) => object.provenance(),
            Self::Segment(object) => object.provenance(),
            Self::Measurement(object) => object.provenance(),
        }
    }

    #[must_use]
    pub fn source_frame(self) -> &'a SourceFrameContext {
        match self {
            Self::Vector(object) => object.source_frame(),
            Self::Segment(object) => object.source_frame(),
            Self::Measurement(object) => object.source_frame(),
        }
    }

    #[must_use]
    pub fn geometry_kind(self) -> WorkspaceObjectGeometryKind {
        match self {
            Self::Vector(object) => match object.geometry() {
                VectorFindingGeometry::Point(_) => WorkspaceObjectGeometryKind::Point,
                VectorFindingGeometry::Regions(_) => WorkspaceObjectGeometryKind::Region,
            },
            Self::Segment(_) => WorkspaceObjectGeometryKind::Segmentation,
            Self::Measurement(_) => WorkspaceObjectGeometryKind::Measurement,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ObjectLocation {
    Vector { layer: usize, object: usize },
    Segment { layer: usize, object: usize },
    Measurement { object: usize },
}

pub(super) enum WorkspaceObjectMut<'a> {
    Vector(&'a mut VectorFinding),
    Segment(&'a mut SegmentationSegmentFinding),
    Measurement(&'a mut WorkspaceLinearMeasurement),
}

impl WorkspaceObjectMut<'_> {
    pub(super) fn set_class_id(&mut self, class_id: String) {
        match self {
            Self::Vector(object) => object.set_class_id(class_id),
            Self::Segment(object) => object.set_class_id(class_id),
            Self::Measurement(object) => object.set_class_id(class_id),
        }
    }

    pub(super) fn finding_site(&self) -> Option<&ControlledFindingSite> {
        match self {
            Self::Vector(object) => object.finding_site(),
            Self::Segment(object) => object.finding_site(),
            Self::Measurement(object) => object.finding_site(),
        }
    }

    pub(super) fn set_finding_site(&mut self, site: Option<ControlledFindingSite>) {
        match self {
            Self::Vector(object) => object.set_finding_site(site),
            Self::Segment(object) => object.set_finding_site(site),
            Self::Measurement(object) => object.set_finding_site(site),
        }
    }

    pub(super) fn name(&self) -> Option<&str> {
        match self {
            Self::Vector(object) => object.name(),
            Self::Segment(object) => object.name(),
            Self::Measurement(object) => object.name(),
        }
    }

    pub(super) fn set_name(&mut self, name: Option<String>) {
        match self {
            Self::Vector(object) => object.set_name(name),
            Self::Segment(object) => object.set_name(name),
            Self::Measurement(object) => object.set_name(name),
        }
    }

    pub(super) fn comment(&self) -> Option<&str> {
        match self {
            Self::Vector(object) => object.comment(),
            Self::Segment(object) => object.comment(),
            Self::Measurement(object) => object.comment(),
        }
    }

    pub(super) fn set_comment(&mut self, comment: Option<String>) {
        match self {
            Self::Vector(object) => object.set_comment(comment),
            Self::Segment(object) => object.set_comment(comment),
            Self::Measurement(object) => object.set_comment(comment),
        }
    }
}

impl WorkspaceDocument {
    #[must_use]
    pub fn object(&self, object_id: Uuid) -> Option<WorkspaceObjectRef<'_>> {
        match self.locate_object(object_id)? {
            ObjectLocation::Vector { layer, object } => Some(WorkspaceObjectRef::Vector(
                &self.vector_layers[layer].findings()[object],
            )),
            ObjectLocation::Segment { layer, object } => Some(WorkspaceObjectRef::Segment(
                &self.segmentation_layers[layer].segments()[object],
            )),
            ObjectLocation::Measurement { object } => {
                Some(WorkspaceObjectRef::Measurement(&self.measurements[object]))
            }
        }
    }

    pub fn objects(&self) -> impl Iterator<Item = WorkspaceObjectRef<'_>> {
        self.vector_findings()
            .map(WorkspaceObjectRef::Vector)
            .chain(self.segments().map(WorkspaceObjectRef::Segment))
            .chain(
                self.measurements
                    .iter()
                    .map(WorkspaceObjectRef::Measurement),
            )
    }

    pub fn object_layer_id(&self, object_id: Uuid) -> Result<Option<Uuid>> {
        match self.locate_object(object_id) {
            Some(ObjectLocation::Vector { layer, .. }) => Ok(Some(self.vector_layers[layer].id())),
            Some(ObjectLocation::Segment { layer, .. }) => {
                Ok(Some(self.segmentation_layers[layer].id()))
            }
            Some(ObjectLocation::Measurement { .. }) => Ok(None),
            None => Err(ViewerError::InvalidInput(
                "the selected workspace object does not exist".into(),
            )),
        }
    }

    #[must_use]
    pub fn finding(&self, object_id: Uuid) -> Option<&VectorFinding> {
        match self.object(object_id) {
            Some(WorkspaceObjectRef::Vector(object)) => Some(object),
            _ => None,
        }
    }

    #[must_use]
    pub fn segment(&self, object_id: Uuid) -> Option<&SegmentationSegmentFinding> {
        match self.object(object_id) {
            Some(WorkspaceObjectRef::Segment(object)) => Some(object),
            _ => None,
        }
    }

    #[must_use]
    pub fn measurement(&self, object_id: Uuid) -> Option<&WorkspaceLinearMeasurement> {
        match self.object(object_id) {
            Some(WorkspaceObjectRef::Measurement(object)) => Some(object),
            _ => None,
        }
    }

    pub fn delete_object(&mut self, object_id: Uuid) -> Result<bool> {
        let Some(location) = self.locate_object(object_id) else {
            return Ok(false);
        };
        match location {
            ObjectLocation::Vector { layer, object } => {
                self.vector_layers[layer].findings_mut().remove(object);
            }
            ObjectLocation::Segment { layer, object } => {
                self.segmentation_layers[layer]
                    .segments_mut()
                    .remove(object);
            }
            ObjectLocation::Measurement { object } => {
                self.measurements.remove(object);
            }
        }
        self.presentation.remove_object(object_id);
        self.bump_revision();
        Ok(true)
    }

    pub(super) fn object_mut(&mut self, object_id: Uuid) -> Option<WorkspaceObjectMut<'_>> {
        match self.locate_object(object_id)? {
            ObjectLocation::Vector { layer, object } => Some(WorkspaceObjectMut::Vector(
                &mut self.vector_layers[layer].findings_mut()[object],
            )),
            ObjectLocation::Segment { layer, object } => Some(WorkspaceObjectMut::Segment(
                &mut self.segmentation_layers[layer].segments_mut()[object],
            )),
            ObjectLocation::Measurement { object } => Some(WorkspaceObjectMut::Measurement(
                &mut self.measurements[object],
            )),
        }
    }

    pub(super) fn object_exists(&self, object_id: Uuid) -> bool {
        self.locate_object(object_id).is_some()
    }

    fn locate_object(&self, object_id: Uuid) -> Option<ObjectLocation> {
        for (layer_index, layer) in self.vector_layers.iter().enumerate() {
            if let Some(object) = layer
                .findings()
                .iter()
                .position(|object| object.object_id() == object_id)
            {
                return Some(ObjectLocation::Vector {
                    layer: layer_index,
                    object,
                });
            }
        }
        for (layer_index, layer) in self.segmentation_layers.iter().enumerate() {
            if let Some(object) = layer
                .segments()
                .iter()
                .position(|object| object.object_id() == object_id)
            {
                return Some(ObjectLocation::Segment {
                    layer: layer_index,
                    object,
                });
            }
        }
        self.measurements
            .iter()
            .position(|object| object.object_id() == object_id)
            .map(|object| ObjectLocation::Measurement { object })
    }
}
