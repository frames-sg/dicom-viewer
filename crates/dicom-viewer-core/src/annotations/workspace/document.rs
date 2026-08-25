use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AnnotationClassGeometry, AnnotationScheme, DicomCode, Point2, Result, TrackingIdentity,
    ViewerError, ViewerSourceIdentity,
};

use super::composition::{
    compose_segment, composite_geometry, erase_intersects, validate_primitive,
};
use super::model::{
    CompositeSegmentGeometry, ControlledFindingSite, ExternalLayerReference,
    ExternalPromotionSource, LayerPresentation, SegmentOperation, SegmentationLayer,
    SegmentationPrimitive, SegmentationSegmentFinding, SourceFrameContext, VectorFinding,
    VectorFindingGeometry, VectorLayer, WorkspaceLinearMeasurement, WorkspaceObjectIdentity,
    WorkspaceObjectProvenance, WorkspacePresentation,
};

const WORKSPACE_SCHEMA_VERSION: u32 = 1;
const MAX_WORKSPACE_BYTES: usize = 128 * 1024 * 1024;
const MAX_EDITABLE_OBJECTS: usize = 100_000;
const MAX_COORDINATE_POINTS: usize = 5_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentEditOutcome {
    Applied,
    NoIntersection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDocument {
    schema_version: u32,
    source_identity: ViewerSourceIdentity,
    scheme: AnnotationScheme,
    next_ordinal: u64,
    revision: u64,
    vector_layers: Vec<VectorLayer>,
    segmentation_layers: Vec<SegmentationLayer>,
    measurements: Vec<WorkspaceLinearMeasurement>,
    external_layers: Vec<ExternalLayerReference>,
    presentation: WorkspacePresentation,
}

impl WorkspaceDocument {
    pub fn new(source_identity: ViewerSourceIdentity, scheme: AnnotationScheme) -> Result<Self> {
        validate_source_identity(&source_identity)?;
        let vector_layer = VectorLayer::new("Findings");
        let mut presentation = WorkspacePresentation::default();
        presentation.insert_layer(vector_layer.id());
        Ok(Self {
            schema_version: WORKSPACE_SCHEMA_VERSION,
            source_identity,
            scheme,
            next_ordinal: 1,
            revision: 0,
            vector_layers: vec![vector_layer],
            segmentation_layers: Vec::new(),
            measurements: Vec::new(),
            external_layers: Vec::new(),
            presentation,
        })
    }

    pub fn from_json(json: &[u8]) -> Result<Self> {
        if json.len() > MAX_WORKSPACE_BYTES {
            return Err(ViewerError::InvalidInput(
                "workspace revision exceeds the 128 MiB limit".into(),
            ));
        }
        wsi_dicom_annotations::validate_unique_object_keys(json, "workspace revision")?;
        let document: Self = serde_json::from_slice(json).map_err(|error| {
            ViewerError::InvalidInput(format!("workspace revision is not valid JSON: {error}"))
        })?;
        document.validate()?;
        Ok(document)
    }

    pub fn to_json(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let json = serde_json::to_vec_pretty(self).map_err(|error| {
            ViewerError::InvalidInput(format!("workspace could not be serialized: {error}"))
        })?;
        if json.len() > MAX_WORKSPACE_BYTES {
            return Err(ViewerError::InvalidInput(
                "workspace revision exceeds the 128 MiB limit".into(),
            ));
        }
        Ok(json)
    }

    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub fn source_identity(&self) -> &ViewerSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub fn scheme(&self) -> &AnnotationScheme {
        &self.scheme
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn vector_layers(&self) -> &[VectorLayer] {
        &self.vector_layers
    }

    #[must_use]
    pub fn segmentation_layers(&self) -> &[SegmentationLayer] {
        &self.segmentation_layers
    }

    #[must_use]
    pub fn measurements(&self) -> &[WorkspaceLinearMeasurement] {
        &self.measurements
    }

    #[must_use]
    pub fn external_layers(&self) -> &[ExternalLayerReference] {
        &self.external_layers
    }

    #[must_use]
    pub fn presentation(&self) -> &WorkspacePresentation {
        &self.presentation
    }

    pub fn vector_findings(&self) -> impl Iterator<Item = &VectorFinding> {
        self.vector_layers
            .iter()
            .flat_map(|layer| layer.findings().iter())
    }

    pub fn segments(&self) -> impl Iterator<Item = &SegmentationSegmentFinding> {
        self.segmentation_layers
            .iter()
            .flat_map(|layer| layer.segments().iter())
    }

    #[must_use]
    pub fn finding(&self, object_id: Uuid) -> Option<&VectorFinding> {
        self.vector_findings()
            .find(|finding| finding.object_id() == object_id)
    }

    #[must_use]
    pub fn segment(&self, object_id: Uuid) -> Option<&SegmentationSegmentFinding> {
        self.segments()
            .find(|segment| segment.object_id() == object_id)
    }

    #[must_use]
    pub fn measurement(&self, object_id: Uuid) -> Option<&WorkspaceLinearMeasurement> {
        self.measurements
            .iter()
            .find(|measurement| measurement.object_id() == object_id)
    }

    pub fn add_vector_finding(
        &mut self,
        layer_id: Uuid,
        class_id: &str,
        geometry: VectorFindingGeometry,
    ) -> Result<Uuid> {
        self.add_vector_finding_internal(
            layer_id,
            class_id,
            geometry,
            WorkspaceObjectProvenance::Manual,
            SourceFrameContext::default(),
            None,
        )
    }

    pub fn promote_vector_finding(
        &mut self,
        layer_id: Uuid,
        class_id: &str,
        geometry: VectorFindingGeometry,
        source: ExternalPromotionSource,
    ) -> Result<Uuid> {
        let (source_layer_id, source_object_id, source_tracking, source_frame) =
            source.into_parts();
        self.add_vector_finding_internal(
            layer_id,
            class_id,
            geometry,
            WorkspaceObjectProvenance::Promoted {
                source_layer_id,
                source_object_id,
            },
            source_frame,
            source_tracking,
        )
    }

    fn add_vector_finding_internal(
        &mut self,
        layer_id: Uuid,
        class_id: &str,
        geometry: VectorFindingGeometry,
        provenance: WorkspaceObjectProvenance,
        source_frame: SourceFrameContext,
        source_tracking: Option<TrackingIdentity>,
    ) -> Result<Uuid> {
        if !self
            .vector_layers
            .iter()
            .any(|layer| layer.id() == layer_id)
        {
            return Err(ViewerError::InvalidInput(
                "the selected vector layer does not exist".into(),
            ));
        }
        let required_geometry = match &geometry {
            VectorFindingGeometry::Point(_) => AnnotationClassGeometry::Point,
            VectorFindingGeometry::Regions(_) => AnnotationClassGeometry::Region,
        };
        self.validate_class(class_id, required_geometry)?;
        validate_vector_geometry(&geometry, self.source_identity.dimensions())?;

        let identity = self.allocate_identity("F", source_tracking.as_ref())?;
        let object_id = identity.object_id;
        let finding = VectorFinding::new(
            identity,
            class_id.to_owned(),
            provenance,
            source_frame,
            geometry,
        );
        self.vector_layers
            .iter_mut()
            .find(|layer| layer.id() == layer_id)
            .expect("the vector layer was checked before allocating identity")
            .findings_mut()
            .push(finding);
        self.bump_revision();
        Ok(object_id)
    }

    pub fn ensure_manual_segmentation_layer(&mut self) -> Uuid {
        if let Some(layer) = self
            .segmentation_layers
            .iter()
            .find(|layer| layer.name() == "Manual Segmentation")
        {
            return layer.id();
        }
        let layer = SegmentationLayer::new("Manual Segmentation");
        let id = layer.id();
        self.presentation.insert_layer(id);
        self.segmentation_layers.push(layer);
        self.bump_revision();
        id
    }

    pub fn add_segment(
        &mut self,
        layer_id: Uuid,
        class_id: &str,
        initial: SegmentationPrimitive,
    ) -> Result<Uuid> {
        self.add_segment_internal(
            layer_id,
            class_id,
            vec![initial],
            WorkspaceObjectProvenance::Manual,
            SourceFrameContext::default(),
            None,
        )
    }

    pub fn promote_segment(
        &mut self,
        layer_id: Uuid,
        class_id: &str,
        primitives: Vec<SegmentationPrimitive>,
        source: ExternalPromotionSource,
    ) -> Result<Uuid> {
        let (source_layer_id, source_object_id, source_tracking, source_frame) =
            source.into_parts();
        if !self
            .external_layers
            .iter()
            .any(|layer| layer.id() == source_layer_id)
        {
            return Err(ViewerError::InvalidInput(
                "promoted segment references an unknown source layer".into(),
            ));
        }
        self.add_segment_internal(
            layer_id,
            class_id,
            primitives,
            WorkspaceObjectProvenance::Promoted {
                source_layer_id,
                source_object_id,
            },
            source_frame,
            source_tracking.as_ref(),
        )
    }

    fn add_segment_internal(
        &mut self,
        layer_id: Uuid,
        class_id: &str,
        primitives: Vec<SegmentationPrimitive>,
        provenance: WorkspaceObjectProvenance,
        source_frame: SourceFrameContext,
        source_tracking: Option<&TrackingIdentity>,
    ) -> Result<Uuid> {
        if !self
            .segmentation_layers
            .iter()
            .any(|layer| layer.id() == layer_id)
        {
            return Err(ViewerError::InvalidInput(
                "the selected segmentation layer does not exist".into(),
            ));
        }
        self.validate_class(class_id, AnnotationClassGeometry::Region)?;
        let Some(initial) = primitives.first() else {
            return Err(ViewerError::InvalidInput(
                "a segmentation segment needs at least one primitive".into(),
            ));
        };
        if initial.operation() != SegmentOperation::Add {
            return Err(ViewerError::InvalidInput(
                "a new segmentation segment must start with an Add primitive".into(),
            ));
        }
        for primitive in &primitives {
            validate_primitive(primitive)?;
        }
        if compose_segment(&primitives, self.source_identity.dimensions())?
            .0
            .is_empty()
        {
            return Err(ViewerError::InvalidInput(
                "the initial segment primitive is outside the source bounds".into(),
            ));
        }

        let identity = self.allocate_identity("S", source_tracking)?;
        let object_id = identity.object_id;
        let mut primitive_iter = primitives.into_iter();
        let first = primitive_iter
            .next()
            .expect("the non-empty primitive list was checked");
        let mut segment = SegmentationSegmentFinding::new(
            identity,
            class_id.to_owned(),
            provenance,
            source_frame,
            first,
        );
        for primitive in primitive_iter {
            segment.push_primitive(primitive);
        }
        self.segmentation_layers
            .iter_mut()
            .find(|layer| layer.id() == layer_id)
            .expect("the segmentation layer was checked before allocating identity")
            .segments_mut()
            .push(segment);
        self.bump_revision();
        Ok(object_id)
    }

    pub fn apply_segment_primitive(
        &mut self,
        segment_id: Uuid,
        primitive: SegmentationPrimitive,
    ) -> Result<SegmentEditOutcome> {
        validate_primitive(&primitive)?;
        let dimensions = self.source_identity.dimensions();
        let current_segment = self.segment(segment_id).ok_or_else(|| {
            ViewerError::InvalidInput("the selected segmentation segment does not exist".into())
        })?;
        let current = compose_segment(current_segment.primitives(), dimensions)?;
        if primitive.operation() == SegmentOperation::Erase
            && !erase_intersects(&current, &primitive, dimensions)?
        {
            return Ok(SegmentEditOutcome::NoIntersection);
        }

        self.segmentation_layers
            .iter_mut()
            .flat_map(|layer| layer.segments_mut().iter_mut())
            .find(|segment| segment.object_id() == segment_id)
            .expect("the segment was checked before mutation")
            .push_primitive(primitive);
        self.bump_revision();
        Ok(SegmentEditOutcome::Applied)
    }

    pub fn move_segment_primitive_point(
        &mut self,
        segment_id: Uuid,
        primitive_index: usize,
        point_index: usize,
        point: Point2,
    ) -> Result<()> {
        validate_point_in_bounds(point, self.source_identity.dimensions())?;
        let segment = self.segment(segment_id).ok_or_else(|| {
            ViewerError::InvalidInput("the selected segmentation segment does not exist".into())
        })?;
        let mut primitive = segment
            .primitives()
            .get(primitive_index)
            .cloned()
            .ok_or_else(|| {
                ViewerError::InvalidInput("segment primitive index is outside the segment".into())
            })?;
        primitive.move_point(point_index, point)?;
        validate_primitive(&primitive)?;
        let mut candidate_primitives = segment.primitives().to_vec();
        candidate_primitives[primitive_index] = primitive.clone();
        if compose_segment(&candidate_primitives, self.source_identity.dimensions())?
            .0
            .is_empty()
        {
            return Err(ViewerError::InvalidInput(
                "segment edit would leave the tracked segment empty".into(),
            ));
        }
        self.segmentation_layers
            .iter_mut()
            .flat_map(|layer| layer.segments_mut())
            .find(|segment| segment.object_id() == segment_id)
            .expect("the segment was checked before mutation")
            .set_primitive(primitive_index, primitive)?;
        self.bump_revision();
        Ok(())
    }

    pub fn composite_segment(&self, segment_id: Uuid) -> Result<CompositeSegmentGeometry> {
        let segment = self.segment(segment_id).ok_or_else(|| {
            ViewerError::InvalidInput("the selected segmentation segment does not exist".into())
        })?;
        Ok(composite_geometry(compose_segment(
            segment.primitives(),
            self.source_identity.dimensions(),
        )?))
    }

    pub fn add_linear_measurement(
        &mut self,
        class_id: &str,
        endpoints: [Point2; 2],
        physical_length_mm: Option<f64>,
    ) -> Result<Uuid> {
        self.add_linear_measurement_internal(
            class_id,
            endpoints,
            physical_length_mm,
            WorkspaceObjectProvenance::Manual,
            SourceFrameContext::default(),
            None,
        )
    }

    pub fn promote_linear_measurement(
        &mut self,
        class_id: &str,
        endpoints: [Point2; 2],
        physical_length_mm: Option<f64>,
        source: ExternalPromotionSource,
    ) -> Result<Uuid> {
        let (source_layer_id, source_object_id, source_tracking, source_frame) =
            source.into_parts();
        if !self
            .external_layers
            .iter()
            .any(|layer| layer.id() == source_layer_id)
        {
            return Err(ViewerError::InvalidInput(
                "promoted measurement references an unknown source layer".into(),
            ));
        }
        self.add_linear_measurement_internal(
            class_id,
            endpoints,
            physical_length_mm,
            WorkspaceObjectProvenance::Promoted {
                source_layer_id,
                source_object_id,
            },
            source_frame,
            source_tracking.as_ref(),
        )
    }

    fn add_linear_measurement_internal(
        &mut self,
        class_id: &str,
        endpoints: [Point2; 2],
        physical_length_mm: Option<f64>,
        provenance: WorkspaceObjectProvenance,
        source_frame: SourceFrameContext,
        source_tracking: Option<&TrackingIdentity>,
    ) -> Result<Uuid> {
        self.validate_class(class_id, AnnotationClassGeometry::Region)?;
        validate_measurement(
            endpoints,
            physical_length_mm,
            self.source_identity.dimensions(),
        )?;
        let identity = self.allocate_identity("M", source_tracking)?;
        let object_id = identity.object_id;
        self.measurements.push(WorkspaceLinearMeasurement::new(
            identity,
            class_id.to_owned(),
            provenance,
            source_frame,
            endpoints,
            physical_length_mm,
        ));
        self.bump_revision();
        Ok(object_id)
    }

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

    /// Updates an unloaded external-layer reference after its payload has been
    /// validated and loaded. The layer identity and any class mappings remain
    /// stable so a discovered sidecar cannot turn into a duplicate layer.
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

    pub fn delete_object(&mut self, object_id: Uuid) -> Result<bool> {
        for layer in &mut self.vector_layers {
            if let Some(index) = layer
                .findings()
                .iter()
                .position(|finding| finding.object_id() == object_id)
            {
                layer.findings_mut().remove(index);
                self.presentation.remove_object(object_id);
                self.bump_revision();
                return Ok(true);
            }
        }
        for layer in &mut self.segmentation_layers {
            if let Some(index) = layer
                .segments()
                .iter()
                .position(|segment| segment.object_id() == object_id)
            {
                layer.segments_mut().remove(index);
                self.presentation.remove_object(object_id);
                self.bump_revision();
                return Ok(true);
            }
        }
        if let Some(index) = self
            .measurements
            .iter()
            .position(|measurement| measurement.object_id() == object_id)
        {
            self.measurements.remove(index);
            self.presentation.remove_object(object_id);
            self.bump_revision();
            return Ok(true);
        }
        Ok(false)
    }

    pub fn move_vector_vertex(
        &mut self,
        object_id: Uuid,
        component_index: usize,
        vertex_index: usize,
        point: Point2,
    ) -> Result<()> {
        let finding = self.finding(object_id).ok_or_else(|| {
            ViewerError::InvalidInput("the selected vector finding does not exist".into())
        })?;
        let VectorFindingGeometry::Regions(components) = finding.geometry() else {
            return Err(ViewerError::InvalidInput(
                "point findings do not have polygon vertices".into(),
            ));
        };
        let mut components = components.clone();
        let component = components.get(component_index).ok_or_else(|| {
            ViewerError::InvalidInput("polygon component index is outside the finding".into())
        })?;
        let mut edited = component.to_vec();
        let vertex = edited.get_mut(vertex_index).ok_or_else(|| {
            ViewerError::InvalidInput("polygon vertex index is outside the finding".into())
        })?;
        *vertex = point;
        components[component_index] = Arc::from(edited);
        let geometry = VectorFindingGeometry::Regions(components);
        validate_vector_geometry(&geometry, self.source_identity.dimensions())?;
        self.vector_layers
            .iter_mut()
            .flat_map(|layer| layer.findings_mut())
            .find(|finding| finding.object_id() == object_id)
            .expect("the finding was checked before mutation")
            .set_geometry(geometry);
        self.bump_revision();
        Ok(())
    }

    pub fn replace_vector_geometry(
        &mut self,
        object_id: Uuid,
        geometry: VectorFindingGeometry,
    ) -> Result<()> {
        let finding = self.finding(object_id).ok_or_else(|| {
            ViewerError::InvalidInput("the selected vector finding does not exist".into())
        })?;
        let expected = match finding.geometry() {
            VectorFindingGeometry::Point(_) => AnnotationClassGeometry::Point,
            VectorFindingGeometry::Regions(_) => AnnotationClassGeometry::Region,
        };
        let replacement = match &geometry {
            VectorFindingGeometry::Point(_) => AnnotationClassGeometry::Point,
            VectorFindingGeometry::Regions(_) => AnnotationClassGeometry::Region,
        };
        if expected != replacement {
            return Err(ViewerError::InvalidInput(
                "geometry replacement cannot change a finding between point and region".into(),
            ));
        }
        validate_vector_geometry(&geometry, self.source_identity.dimensions())?;
        self.vector_layers
            .iter_mut()
            .flat_map(|layer| layer.findings_mut())
            .find(|finding| finding.object_id() == object_id)
            .expect("the finding was checked before mutation")
            .set_geometry(geometry);
        self.bump_revision();
        Ok(())
    }

    pub fn set_measurement_endpoints(
        &mut self,
        object_id: Uuid,
        endpoints: [Point2; 2],
        physical_length_mm: Option<f64>,
    ) -> Result<()> {
        validate_measurement(
            endpoints,
            physical_length_mm,
            self.source_identity.dimensions(),
        )?;
        let measurement = self
            .measurements
            .iter_mut()
            .find(|measurement| measurement.object_id() == object_id)
            .ok_or_else(|| {
                ViewerError::InvalidInput("the selected measurement does not exist".into())
            })?;
        measurement.set_endpoints(endpoints, physical_length_mm);
        self.bump_revision();
        Ok(())
    }

    pub fn reclassify_object(&mut self, object_id: Uuid, class_id: &str) -> Result<()> {
        let geometry = if let Some(finding) = self.finding(object_id) {
            match finding.geometry() {
                VectorFindingGeometry::Point(_) => AnnotationClassGeometry::Point,
                VectorFindingGeometry::Regions(_) => AnnotationClassGeometry::Region,
            }
        } else if self.segment(object_id).is_some() || self.measurement(object_id).is_some() {
            AnnotationClassGeometry::Region
        } else {
            return Err(ViewerError::InvalidInput(
                "the selected workspace object does not exist".into(),
            ));
        };
        self.validate_class(class_id, geometry)?;
        if let Some(finding) = self
            .vector_layers
            .iter_mut()
            .flat_map(|layer| layer.findings_mut())
            .find(|finding| finding.object_id() == object_id)
        {
            finding.set_class_id(class_id.to_owned());
        } else if let Some(segment) = self
            .segmentation_layers
            .iter_mut()
            .flat_map(|layer| layer.segments_mut())
            .find(|segment| segment.object_id() == object_id)
        {
            segment.set_class_id(class_id.to_owned());
        } else if let Some(measurement) = self
            .measurements
            .iter_mut()
            .find(|measurement| measurement.object_id() == object_id)
        {
            measurement.set_class_id(class_id.to_owned());
        }
        self.bump_revision();
        Ok(())
    }

    /// Sets the optional user-facing name on one tracked object.
    ///
    /// Names are object metadata, never annotation-class aliases. Supplying
    /// `None` clears the name; an explicit empty or whitespace-only name is
    /// rejected so serialized workspaces remain unambiguous.
    pub fn set_object_finding_site(
        &mut self,
        object_id: Uuid,
        site: Option<&DicomCode>,
    ) -> Result<()> {
        let site = site
            .map(|candidate| {
                self.scheme
                    .finding_sites()
                    .iter()
                    .find(|controlled| {
                        ControlledFindingSite::from_code(controlled).matches(candidate)
                    })
                    .map(ControlledFindingSite::from_code)
                    .ok_or_else(|| {
                        ViewerError::InvalidInput(
                            "finding site is not controlled by the pinned annotation scheme".into(),
                        )
                    })
            })
            .transpose()?;
        if let Some(finding) = self
            .vector_layers
            .iter_mut()
            .flat_map(|layer| layer.findings_mut())
            .find(|finding| finding.object_id() == object_id)
        {
            if finding.finding_site() != site.as_ref() {
                finding.set_finding_site(site);
                self.bump_revision();
            }
            return Ok(());
        }
        if let Some(segment) = self
            .segmentation_layers
            .iter_mut()
            .flat_map(|layer| layer.segments_mut())
            .find(|segment| segment.object_id() == object_id)
        {
            if segment.finding_site() != site.as_ref() {
                segment.set_finding_site(site);
                self.bump_revision();
            }
            return Ok(());
        }
        if let Some(measurement) = self
            .measurements
            .iter_mut()
            .find(|measurement| measurement.object_id() == object_id)
        {
            if measurement.finding_site() != site.as_ref() {
                measurement.set_finding_site(site);
                self.bump_revision();
            }
            return Ok(());
        }
        Err(ViewerError::InvalidInput(
            "the selected workspace object does not exist".into(),
        ))
    }

    pub fn set_object_name(&mut self, object_id: Uuid, name: Option<&str>) -> Result<()> {
        let name = validate_optional_object_text(name, 256, "object name")?;
        if let Some(finding) = self
            .vector_layers
            .iter_mut()
            .flat_map(|layer| layer.findings_mut())
            .find(|finding| finding.object_id() == object_id)
        {
            if finding.name() != name.as_deref() {
                finding.set_name(name);
                self.bump_revision();
            }
            return Ok(());
        }
        if let Some(segment) = self
            .segmentation_layers
            .iter_mut()
            .flat_map(|layer| layer.segments_mut())
            .find(|segment| segment.object_id() == object_id)
        {
            if segment.name() != name.as_deref() {
                segment.set_name(name);
                self.bump_revision();
            }
            return Ok(());
        }
        if let Some(measurement) = self
            .measurements
            .iter_mut()
            .find(|measurement| measurement.object_id() == object_id)
        {
            if measurement.name() != name.as_deref() {
                measurement.set_name(name);
                self.bump_revision();
            }
            return Ok(());
        }
        Err(ViewerError::InvalidInput(
            "the selected workspace object does not exist".into(),
        ))
    }

    /// Sets the optional comment on one tracked object.
    pub fn set_object_comment(&mut self, object_id: Uuid, comment: Option<&str>) -> Result<()> {
        let comment = validate_optional_object_text(comment, 4_096, "object comment")?;
        if let Some(finding) = self
            .vector_layers
            .iter_mut()
            .flat_map(|layer| layer.findings_mut())
            .find(|finding| finding.object_id() == object_id)
        {
            if finding.comment() != comment.as_deref() {
                finding.set_comment(comment);
                self.bump_revision();
            }
            return Ok(());
        }
        if let Some(segment) = self
            .segmentation_layers
            .iter_mut()
            .flat_map(|layer| layer.segments_mut())
            .find(|segment| segment.object_id() == object_id)
        {
            if segment.comment() != comment.as_deref() {
                segment.set_comment(comment);
                self.bump_revision();
            }
            return Ok(());
        }
        if let Some(measurement) = self
            .measurements
            .iter_mut()
            .find(|measurement| measurement.object_id() == object_id)
        {
            if measurement.comment() != comment.as_deref() {
                measurement.set_comment(comment);
                self.bump_revision();
            }
            return Ok(());
        }
        Err(ViewerError::InvalidInput(
            "the selected workspace object does not exist".into(),
        ))
    }

    pub fn suggest_scheme_migration(&self, target: &AnnotationScheme) -> BTreeMap<String, String> {
        let mut suggestions = BTreeMap::new();
        for class_id in self.used_class_ids() {
            let Some(source_class) = self.scheme.class(&class_id) else {
                continue;
            };
            if let Some(target_class) = target.class_for_concept(source_class.concept_key()) {
                suggestions.insert(class_id, target_class.id().to_owned());
            }
        }
        suggestions
    }

    pub fn migrate_scheme(
        &mut self,
        target: AnnotationScheme,
        mappings: &BTreeMap<String, String>,
    ) -> Result<()> {
        let used = self.used_classes_with_geometry()?;
        for (source_id, geometry) in &used {
            let target_id = mappings.get(source_id).ok_or_else(|| {
                ViewerError::InvalidInput(format!(
                    "used class {source_id:?} requires an explicit migration mapping"
                ))
            })?;
            let target_class = target.class(target_id).ok_or_else(|| {
                ViewerError::InvalidInput(format!(
                    "migration target class {target_id:?} does not exist"
                ))
            })?;
            if target_class.geometry() != *geometry {
                return Err(ViewerError::InvalidInput(format!(
                    "migration from {source_id:?} to {target_id:?} changes geometry from {} to {}",
                    geometry.label(),
                    target_class.geometry().label()
                )));
            }
        }
        self.validate_sites_for_scheme(&target)?;

        for finding in self
            .vector_layers
            .iter_mut()
            .flat_map(|layer| layer.findings_mut())
        {
            if let Some(target_id) = mappings.get(finding.class_id()) {
                finding.set_class_id(target_id.clone());
            }
        }
        for segment in self
            .segmentation_layers
            .iter_mut()
            .flat_map(|layer| layer.segments_mut())
        {
            if let Some(target_id) = mappings.get(segment.class_id()) {
                segment.set_class_id(target_id.clone());
            }
        }
        for measurement in &mut self.measurements {
            if let Some(target_id) = mappings.get(measurement.class_id()) {
                measurement.set_class_id(target_id.clone());
            }
        }
        for layer in &mut self.external_layers {
            layer.remap_class_targets(mappings);
        }
        self.scheme = target;
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

    #[must_use]
    pub fn object_count(&self) -> usize {
        self.vector_findings().count() + self.segments().count() + self.measurements.len()
    }

    #[must_use]
    pub fn coordinate_count(&self) -> usize {
        self.vector_findings()
            .map(|finding| finding.geometry().coordinate_count())
            .sum::<usize>()
            + self
                .segments()
                .flat_map(|segment| segment.primitives())
                .map(SegmentationPrimitive::coordinate_count)
                .sum::<usize>()
            + self.measurements.len().saturating_mul(2)
    }

    #[must_use]
    pub fn estimated_retained_bytes(&self) -> usize {
        1024usize
            .saturating_add(self.object_count().saturating_mul(512))
            .saturating_add(self.coordinate_count().saturating_mul(16))
    }

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

    fn validate_class(&self, class_id: &str, geometry: AnnotationClassGeometry) -> Result<()> {
        let class = self.scheme.class(class_id).ok_or_else(|| {
            ViewerError::InvalidInput(format!(
                "workspace object references unknown class {class_id:?}"
            ))
        })?;
        if class.geometry() != geometry {
            return Err(ViewerError::InvalidInput(format!(
                "class {:?} requires {} geometry, not {}",
                class.label(),
                class.geometry().label(),
                geometry.label()
            )));
        }
        Ok(())
    }

    fn used_class_ids(&self) -> Vec<String> {
        let mut ids = self
            .vector_findings()
            .map(|finding| finding.class_id().to_owned())
            .chain(self.segments().map(|segment| segment.class_id().to_owned()))
            .chain(
                self.measurements
                    .iter()
                    .map(|measurement| measurement.class_id().to_owned()),
            )
            .chain(
                self.external_layers
                    .iter()
                    .flat_map(|layer| layer.class_mappings().values().cloned()),
            )
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    fn used_classes_with_geometry(&self) -> Result<BTreeMap<String, AnnotationClassGeometry>> {
        let mut used = BTreeMap::new();
        for class_id in self.used_class_ids() {
            let geometry = self
                .scheme
                .class(&class_id)
                .ok_or_else(|| {
                    ViewerError::InvalidInput(format!(
                        "workspace object references unknown class {class_id:?}"
                    ))
                })?
                .geometry();
            used.insert(class_id, geometry);
        }
        Ok(used)
    }

    fn validate_sites_for_scheme(&self, scheme: &AnnotationScheme) -> Result<()> {
        for site in self
            .vector_findings()
            .filter_map(VectorFinding::finding_site)
            .chain(
                self.segments()
                    .filter_map(SegmentationSegmentFinding::finding_site),
            )
            .chain(
                self.measurements
                    .iter()
                    .filter_map(WorkspaceLinearMeasurement::finding_site),
            )
        {
            if !scheme.finding_sites().iter().any(|code| site.matches(code)) {
                return Err(ViewerError::InvalidInput(
                    "workspace finding site is not controlled by the pinned annotation scheme"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    fn allocate_identity(
        &mut self,
        prefix: &str,
        source_tracking: Option<&TrackingIdentity>,
    ) -> Result<WorkspaceObjectIdentity> {
        let ordinal = self.next_ordinal;
        let tracking = match source_tracking {
            Some(source)
                if TrackingIdentity::new(source.id(), source.uid()).is_ok()
                    && !self.tracking_conflicts(source) =>
            {
                source.clone()
            }
            _ => TrackingIdentity::generated(prefix, ordinal)?,
        };
        self.next_ordinal = self.next_ordinal.checked_add(1).ok_or_else(|| {
            ViewerError::InvalidInput("workspace ordinal space is exhausted".into())
        })?;
        Ok(WorkspaceObjectIdentity {
            object_id: Uuid::new_v4(),
            ordinal,
            tracking,
        })
    }

    fn tracking_conflicts(&self, candidate: &TrackingIdentity) -> bool {
        self.vector_findings()
            .map(VectorFinding::tracking)
            .chain(self.segments().map(SegmentationSegmentFinding::tracking))
            .chain(
                self.measurements
                    .iter()
                    .map(WorkspaceLinearMeasurement::tracking),
            )
            .any(|existing| existing.id() == candidate.id() || existing.uid() == candidate.uid())
    }

    fn layer_exists(&self, id: Uuid) -> bool {
        self.vector_layers.iter().any(|layer| layer.id() == id)
            || self
                .segmentation_layers
                .iter()
                .any(|layer| layer.id() == id)
            || self.external_layers.iter().any(|layer| layer.id() == id)
    }

    fn object_exists(&self, id: Uuid) -> bool {
        self.finding(id).is_some() || self.segment(id).is_some() || self.measurement(id).is_some()
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

fn validate_source_identity(identity: &ViewerSourceIdentity) -> Result<()> {
    let (z, c, t) = identity.plane();
    let expected = ViewerSourceIdentity::new(
        identity.dataset_id(),
        identity.scene(),
        identity.series(),
        z,
        c,
        t,
        identity.dimensions(),
    );
    if identity.dimensions().0 == 0
        || identity.dimensions().1 == 0
        || identity.digest() != expected.digest()
    {
        return Err(ViewerError::InvalidInput(
            "workspace source identity is inconsistent".into(),
        ));
    }
    Ok(())
}

fn validate_vector_geometry(
    geometry: &VectorFindingGeometry,
    dimensions: (u64, u64),
) -> Result<()> {
    match geometry {
        VectorFindingGeometry::Point(point) => validate_point_in_bounds(*point, dimensions),
        VectorFindingGeometry::Regions(components) => {
            if components.is_empty() {
                return Err(ViewerError::InvalidInput(
                    "a region finding needs at least one polygon component".into(),
                ));
            }
            for component in components {
                wsi_dicom_annotations::validate_polygon(component)?;
                for point in component.iter() {
                    validate_point_in_bounds(*point, dimensions)?;
                }
            }
            Ok(())
        }
    }
}

fn validate_measurement(
    endpoints: [Point2; 2],
    physical_length_mm: Option<f64>,
    dimensions: (u64, u64),
) -> Result<()> {
    for endpoint in endpoints {
        validate_point_in_bounds(endpoint, dimensions)?;
    }
    if endpoints[0] == endpoints[1] {
        return Err(ViewerError::InvalidInput(
            "a linear measurement needs two distinct endpoints".into(),
        ));
    }
    if physical_length_mm.is_some_and(|length| !length.is_finite() || length <= 0.0) {
        return Err(ViewerError::InvalidInput(
            "physical measurement length must be finite and positive".into(),
        ));
    }
    Ok(())
}

fn validate_optional_object_text(
    value: Option<&str>,
    maximum_bytes: usize,
    field: &str,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() || value.len() > maximum_bytes || value.contains('\0') {
        return Err(ViewerError::InvalidInput(format!(
            "{field} must be 1..={maximum_bytes} bytes and contain no NUL"
        )));
    }
    Ok(Some(trimmed.to_owned()))
}

fn validate_point_in_bounds(point: Point2, dimensions: (u64, u64)) -> Result<()> {
    if !point.x.is_finite()
        || !point.y.is_finite()
        || point.x < 0.0
        || point.y < 0.0
        || point.x > dimensions.0 as f64
        || point.y > dimensions.1 as f64
    {
        return Err(ViewerError::InvalidInput(
            "workspace coordinate is outside the source bounds".into(),
        ));
    }
    Ok(())
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
