use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{DicomCode, DicomCodeValueKind, Point2, Result, TrackingIdentity, ViewerError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlledFindingSite {
    value: String,
    value_kind: DicomCodeValueKind,
    scheme: String,
    coding_scheme_version: Option<String>,
}

impl ControlledFindingSite {
    #[must_use]
    pub fn from_code(code: &DicomCode) -> Self {
        Self {
            value: code.value().to_owned(),
            value_kind: code.value_kind(),
            scheme: code.scheme().to_owned(),
            coding_scheme_version: code.coding_scheme_version().map(str::to_owned),
        }
    }

    #[must_use]
    pub fn matches(&self, code: &DicomCode) -> bool {
        self.value == code.value()
            && self.value_kind == code.value_kind()
            && self.scheme == code.scheme()
            && self.coding_scheme_version.as_deref() == code.coding_scheme_version()
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFrameContext {
    optical_path: Option<String>,
    z: Option<u32>,
    c: Option<u32>,
    t: Option<u32>,
}

impl SourceFrameContext {
    #[must_use]
    pub fn new(
        optical_path: Option<String>,
        z: Option<u32>,
        c: Option<u32>,
        t: Option<u32>,
    ) -> Self {
        Self {
            optical_path,
            z,
            c,
            t,
        }
    }

    #[must_use]
    pub fn optical_path(&self) -> Option<&str> {
        self.optical_path.as_deref()
    }

    #[must_use]
    pub const fn plane(&self) -> (Option<u32>, Option<u32>, Option<u32>) {
        (self.z, self.c, self.t)
    }

    /// Converts ANN group applicability into a promotable workspace source context.
    ///
    /// Groups applying to multiple optical paths remain readable but cannot be promoted into one
    /// editable object because the workspace object model carries at most one optical path.
    pub fn from_ann_group(group: &wsi_dicom_annotations::AnnotationGroup) -> Result<Self> {
        if !group.applies_to_all_z_planes() || !group.common_z_coordinates().is_empty() {
            return Err(ViewerError::Unsupported(
                "ANN group has Z-plane applicability that one editable workspace object cannot preserve"
                    .into(),
            ));
        }
        let optical_path = match (
            group.applies_to_all_optical_paths(),
            group.referenced_optical_paths(),
        ) {
            (true, []) => None,
            (false, [identifier]) => Some(identifier.clone()),
            (false, identifiers) if identifiers.len() > 1 => {
                return Err(ViewerError::Unsupported(format!(
                    "ANN group references {} optical paths and remains read-only because promotion would lose applicability",
                    identifiers.len()
                )))
            }
            _ => {
                return Err(ViewerError::InvalidInput(
                    "ANN group has inconsistent optical-path applicability".into(),
                ))
            }
        };
        Ok(Self::new(optical_path, None, None, None))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum WorkspaceObjectProvenance {
    Manual,
    Promoted {
        source_layer_id: Uuid,
        source_object_id: String,
    },
}

/// Identifies and locates one object promoted from a read-only source layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalPromotionSource {
    source_layer_id: Uuid,
    source_object_id: String,
    source_tracking: Option<TrackingIdentity>,
    source_frame: SourceFrameContext,
}

impl ExternalPromotionSource {
    #[must_use]
    pub fn new(
        source_layer_id: Uuid,
        source_object_id: impl Into<String>,
        source_tracking: Option<TrackingIdentity>,
        source_frame: SourceFrameContext,
    ) -> Self {
        Self {
            source_layer_id,
            source_object_id: source_object_id.into(),
            source_tracking,
            source_frame,
        }
    }

    pub(super) fn into_parts(self) -> (Uuid, String, Option<TrackingIdentity>, SourceFrameContext) {
        (
            self.source_layer_id,
            self.source_object_id,
            self.source_tracking,
            self.source_frame,
        )
    }
}

pub(super) struct WorkspaceObjectIdentity {
    pub(super) object_id: Uuid,
    pub(super) ordinal: u64,
    pub(super) tracking: TrackingIdentity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "coordinates",
    rename_all = "SCREAMING_SNAKE_CASE"
)]
pub enum VectorFindingGeometry {
    Point(Point2),
    Regions(Vec<Arc<[Point2]>>),
}

impl VectorFindingGeometry {
    #[must_use]
    pub fn regions(components: Vec<Vec<Point2>>) -> Self {
        Self::Regions(components.into_iter().map(Arc::from).collect())
    }

    #[must_use]
    pub fn coordinate_count(&self) -> usize {
        match self {
            Self::Point(_) => 1,
            Self::Regions(components) => components.iter().map(|component| component.len()).sum(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorFinding {
    object_id: Uuid,
    ordinal: u64,
    tracking: TrackingIdentity,
    class_id: String,
    finding_site: Option<ControlledFindingSite>,
    name: Option<String>,
    comment: Option<String>,
    provenance: WorkspaceObjectProvenance,
    source_frame: SourceFrameContext,
    geometry: VectorFindingGeometry,
}

impl VectorFinding {
    #[must_use]
    pub const fn object_id(&self) -> Uuid {
        self.object_id
    }

    #[must_use]
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }

    #[must_use]
    pub fn tracking(&self) -> &TrackingIdentity {
        &self.tracking
    }

    #[must_use]
    pub fn class_id(&self) -> &str {
        &self.class_id
    }

    #[must_use]
    pub fn finding_site(&self) -> Option<&ControlledFindingSite> {
        self.finding_site.as_ref()
    }

    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[must_use]
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }

    #[must_use]
    pub fn provenance(&self) -> &WorkspaceObjectProvenance {
        &self.provenance
    }

    #[must_use]
    pub fn source_frame(&self) -> &SourceFrameContext {
        &self.source_frame
    }

    #[must_use]
    pub fn geometry(&self) -> &VectorFindingGeometry {
        &self.geometry
    }

    pub(super) fn new(
        identity: WorkspaceObjectIdentity,
        class_id: String,
        provenance: WorkspaceObjectProvenance,
        source_frame: SourceFrameContext,
        geometry: VectorFindingGeometry,
    ) -> Self {
        Self {
            object_id: identity.object_id,
            ordinal: identity.ordinal,
            tracking: identity.tracking,
            class_id,
            finding_site: None,
            name: None,
            comment: None,
            provenance,
            source_frame,
            geometry,
        }
    }

    pub(super) fn set_class_id(&mut self, class_id: String) {
        self.class_id = class_id;
    }

    pub(super) fn set_finding_site(&mut self, site: Option<ControlledFindingSite>) {
        self.finding_site = site;
    }

    pub(super) fn set_geometry(&mut self, geometry: VectorFindingGeometry) {
        self.geometry = geometry;
    }

    pub(super) fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    pub(super) fn set_comment(&mut self, comment: Option<String>) {
        self.comment = comment;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SegmentOperation {
    Add,
    Erase,
}

impl SegmentOperation {
    #[must_use]
    pub const fn reversed(self) -> Self {
        match self {
            Self::Add => Self::Erase,
            Self::Erase => Self::Add,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SegmentationPrimitiveGeometry {
    Polygon {
        points: Arc<[Point2]>,
    },
    Brush {
        centerline: Arc<[Point2]>,
        diameter: f64,
    },
}

impl SegmentationPrimitiveGeometry {
    #[must_use]
    pub fn coordinate_count(&self) -> usize {
        match self {
            Self::Polygon { points } => points.len(),
            Self::Brush { centerline, .. } => centerline.len(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentationPrimitive {
    operation: SegmentOperation,
    geometry: SegmentationPrimitiveGeometry,
}

impl SegmentationPrimitive {
    #[must_use]
    pub fn polygon(operation: SegmentOperation, points: Vec<Point2>) -> Self {
        Self {
            operation,
            geometry: SegmentationPrimitiveGeometry::Polygon {
                points: Arc::from(points),
            },
        }
    }

    #[must_use]
    pub fn brush(operation: SegmentOperation, centerline: Vec<Point2>, diameter: f64) -> Self {
        Self {
            operation,
            geometry: SegmentationPrimitiveGeometry::Brush {
                centerline: Arc::from(centerline),
                diameter,
            },
        }
    }

    #[must_use]
    pub const fn operation(&self) -> SegmentOperation {
        self.operation
    }

    #[must_use]
    pub fn geometry(&self) -> &SegmentationPrimitiveGeometry {
        &self.geometry
    }

    #[must_use]
    pub fn coordinate_count(&self) -> usize {
        self.geometry.coordinate_count()
    }

    pub(super) fn move_point(&mut self, index: usize, point: Point2) -> Result<()> {
        let points = match &mut self.geometry {
            SegmentationPrimitiveGeometry::Polygon { points }
            | SegmentationPrimitiveGeometry::Brush {
                centerline: points, ..
            } => points,
        };
        let mut edited = points.to_vec();
        let target = edited.get_mut(index).ok_or_else(|| {
            ViewerError::InvalidInput(
                "segmentation primitive point index is outside the geometry".into(),
            )
        })?;
        *target = point;
        *points = Arc::from(edited);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentationSegmentFinding {
    object_id: Uuid,
    ordinal: u64,
    tracking: TrackingIdentity,
    class_id: String,
    finding_site: Option<ControlledFindingSite>,
    name: Option<String>,
    comment: Option<String>,
    provenance: WorkspaceObjectProvenance,
    source_frame: SourceFrameContext,
    primitives: Vec<SegmentationPrimitive>,
}

impl SegmentationSegmentFinding {
    #[must_use]
    pub const fn object_id(&self) -> Uuid {
        self.object_id
    }

    #[must_use]
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }

    #[must_use]
    pub fn tracking(&self) -> &TrackingIdentity {
        &self.tracking
    }

    #[must_use]
    pub fn class_id(&self) -> &str {
        &self.class_id
    }

    #[must_use]
    pub fn primitives(&self) -> &[SegmentationPrimitive] {
        &self.primitives
    }

    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[must_use]
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }

    #[must_use]
    pub fn finding_site(&self) -> Option<&ControlledFindingSite> {
        self.finding_site.as_ref()
    }

    #[must_use]
    pub fn provenance(&self) -> &WorkspaceObjectProvenance {
        &self.provenance
    }

    #[must_use]
    pub fn source_frame(&self) -> &SourceFrameContext {
        &self.source_frame
    }

    pub(super) fn new(
        identity: WorkspaceObjectIdentity,
        class_id: String,
        provenance: WorkspaceObjectProvenance,
        source_frame: SourceFrameContext,
        initial: SegmentationPrimitive,
    ) -> Self {
        Self {
            object_id: identity.object_id,
            ordinal: identity.ordinal,
            tracking: identity.tracking,
            class_id,
            finding_site: None,
            name: None,
            comment: None,
            provenance,
            source_frame,
            primitives: vec![initial],
        }
    }

    pub(super) fn set_class_id(&mut self, class_id: String) {
        self.class_id = class_id;
    }

    pub(super) fn set_finding_site(&mut self, site: Option<ControlledFindingSite>) {
        self.finding_site = site;
    }

    pub(super) fn push_primitive(&mut self, primitive: SegmentationPrimitive) {
        self.primitives.push(primitive);
    }

    pub(super) fn set_primitive(
        &mut self,
        index: usize,
        primitive: SegmentationPrimitive,
    ) -> Result<()> {
        let target = self.primitives.get_mut(index).ok_or_else(|| {
            ViewerError::InvalidInput("segment primitive index is outside the segment".into())
        })?;
        *target = primitive;
        Ok(())
    }

    pub(super) fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    pub(super) fn set_comment(&mut self, comment: Option<String>) {
        self.comment = comment;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceLinearMeasurement {
    object_id: Uuid,
    ordinal: u64,
    tracking: TrackingIdentity,
    class_id: String,
    finding_site: Option<ControlledFindingSite>,
    name: Option<String>,
    comment: Option<String>,
    provenance: WorkspaceObjectProvenance,
    source_frame: SourceFrameContext,
    endpoints: [Point2; 2],
    physical_length_mm: Option<f64>,
}

impl WorkspaceLinearMeasurement {
    pub(super) fn new(
        identity: WorkspaceObjectIdentity,
        class_id: String,
        provenance: WorkspaceObjectProvenance,
        source_frame: SourceFrameContext,
        endpoints: [Point2; 2],
        physical_length_mm: Option<f64>,
    ) -> Self {
        Self {
            object_id: identity.object_id,
            ordinal: identity.ordinal,
            tracking: identity.tracking,
            class_id,
            finding_site: None,
            name: None,
            comment: None,
            provenance,
            source_frame,
            endpoints,
            physical_length_mm,
        }
    }

    #[must_use]
    pub const fn object_id(&self) -> Uuid {
        self.object_id
    }

    #[must_use]
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }

    #[must_use]
    pub fn tracking(&self) -> &TrackingIdentity {
        &self.tracking
    }

    #[must_use]
    pub fn class_id(&self) -> &str {
        &self.class_id
    }

    #[must_use]
    pub const fn endpoints(&self) -> [Point2; 2] {
        self.endpoints
    }

    #[must_use]
    pub const fn physical_length_mm(&self) -> Option<f64> {
        self.physical_length_mm
    }

    #[must_use]
    pub fn finding_site(&self) -> Option<&ControlledFindingSite> {
        self.finding_site.as_ref()
    }

    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[must_use]
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }

    #[must_use]
    pub fn provenance(&self) -> &WorkspaceObjectProvenance {
        &self.provenance
    }

    #[must_use]
    pub fn source_frame(&self) -> &SourceFrameContext {
        &self.source_frame
    }

    pub(super) fn set_class_id(&mut self, class_id: String) {
        self.class_id = class_id;
    }

    pub(super) fn set_finding_site(&mut self, site: Option<ControlledFindingSite>) {
        self.finding_site = site;
    }

    pub(super) fn set_endpoints(
        &mut self,
        endpoints: [Point2; 2],
        physical_length_mm: Option<f64>,
    ) {
        self.endpoints = endpoints;
        self.physical_length_mm = physical_length_mm;
    }

    pub(super) fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    pub(super) fn set_comment(&mut self, comment: Option<String>) {
        self.comment = comment;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExternalLayerKind {
    DicomAnn,
    DicomSeg,
    DicomSr,
    ProfiledGeoJson,
    Heatmap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalLayerReference {
    id: Uuid,
    name: String,
    kind: ExternalLayerKind,
    source_path: Option<PathBuf>,
    source_digest: Option<String>,
    source_object_count: u64,
    class_mappings: BTreeMap<String, String>,
}

impl ExternalLayerReference {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        kind: ExternalLayerKind,
        source_path: Option<PathBuf>,
        source_digest: Option<String>,
        source_object_count: u64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            source_path,
            source_digest,
            source_object_count,
            class_mappings: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn kind(&self) -> &ExternalLayerKind {
        &self.kind
    }

    #[must_use]
    pub fn source_path(&self) -> Option<&std::path::Path> {
        self.source_path.as_deref()
    }

    #[must_use]
    pub fn class_mappings(&self) -> &BTreeMap<String, String> {
        &self.class_mappings
    }

    #[must_use]
    pub const fn source_object_count(&self) -> u64 {
        self.source_object_count
    }

    #[must_use]
    pub fn source_digest(&self) -> Option<&str> {
        self.source_digest.as_deref()
    }

    pub(super) fn set_class_mapping(&mut self, source_class: String, target_class: String) {
        self.class_mappings.insert(source_class, target_class);
    }

    pub(super) fn hydrate(&mut self, source_object_count: u64, source_digest: Option<String>) {
        self.source_object_count = source_object_count;
        self.source_digest = source_digest;
    }

    pub(super) fn remap_class_targets(&mut self, mappings: &BTreeMap<String, String>) {
        for target in self.class_mappings.values_mut() {
            if let Some(replacement) = mappings.get(target) {
                *target = replacement.clone();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerPresentation {
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
}

impl Default for LayerPresentation {
    fn default() -> Self {
        Self {
            visible: true,
            locked: false,
            opacity: 1.0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePresentation {
    layers: BTreeMap<Uuid, LayerPresentation>,
    object_visibility: BTreeMap<Uuid, bool>,
    /// Consumes presentation-only colors written by pre-scheme-controlled drafts.
    #[serde(
        default,
        skip_serializing,
        rename = "class_color_overrides",
        deserialize_with = "ignore_legacy_class_colors"
    )]
    _legacy_class_colors: (),
}

fn ignore_legacy_class_colors<'de, D>(deserializer: D) -> std::result::Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::de::IgnoredAny::deserialize(deserializer).map(drop)
}

impl WorkspacePresentation {
    #[must_use]
    pub fn layer(&self, id: Uuid) -> LayerPresentation {
        self.layers.get(&id).copied().unwrap_or_default()
    }

    #[must_use]
    pub fn object_visible(&self, id: Uuid) -> bool {
        self.object_visibility.get(&id).copied().unwrap_or(true)
    }

    pub(super) fn insert_layer(&mut self, id: Uuid) {
        self.layers.entry(id).or_default();
    }

    pub(super) fn remove_layer(&mut self, id: Uuid) {
        self.layers.remove(&id);
    }

    pub(super) fn remove_object(&mut self, id: Uuid) {
        self.object_visibility.remove(&id);
    }

    pub(super) fn set_layer(&mut self, id: Uuid, presentation: LayerPresentation) {
        self.layers.insert(id, presentation);
    }

    pub(super) fn set_object_visible(&mut self, id: Uuid, visible: bool) {
        if visible {
            self.object_visibility.remove(&id);
        } else {
            self.object_visibility.insert(id, false);
        }
    }

    pub(super) fn validate(
        &self,
        layer_ids: &HashSet<Uuid>,
        object_ids: &HashSet<Uuid>,
    ) -> Result<()> {
        for (id, presentation) in &self.layers {
            if !layer_ids.contains(id)
                || !presentation.opacity.is_finite()
                || !(0.0..=1.0).contains(&presentation.opacity)
            {
                return Err(ViewerError::InvalidInput(
                    "workspace contains invalid layer presentation state".into(),
                ));
            }
        }
        if self
            .object_visibility
            .keys()
            .any(|id| !object_ids.contains(id))
        {
            return Err(ViewerError::InvalidInput(
                "workspace presentation references an unknown object".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorLayer {
    id: Uuid,
    name: String,
    findings: Vec<VectorFinding>,
}

impl VectorLayer {
    pub(super) fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            findings: Vec::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn findings(&self) -> &[VectorFinding] {
        &self.findings
    }

    pub(super) fn findings_mut(&mut self) -> &mut Vec<VectorFinding> {
        &mut self.findings
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentationLayer {
    id: Uuid,
    name: String,
    segments: Vec<SegmentationSegmentFinding>,
}

impl SegmentationLayer {
    pub(super) fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            segments: Vec::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn segments(&self) -> &[SegmentationSegmentFinding] {
        &self.segments
    }

    pub(super) fn segments_mut(&mut self) -> &mut Vec<SegmentationSegmentFinding> {
        &mut self.segments
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolygonComponent {
    exterior: Vec<Point2>,
    holes: Vec<Vec<Point2>>,
}

impl PolygonComponent {
    pub(super) fn new(exterior: Vec<Point2>, holes: Vec<Vec<Point2>>) -> Self {
        Self { exterior, holes }
    }

    #[must_use]
    pub fn exterior(&self) -> &[Point2] {
        &self.exterior
    }

    #[must_use]
    pub fn holes(&self) -> &[Vec<Point2>] {
        &self.holes
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompositeSegmentGeometry {
    components: Vec<PolygonComponent>,
}

impl CompositeSegmentGeometry {
    pub(super) fn new(components: Vec<PolygonComponent>) -> Self {
        Self { components }
    }

    #[must_use]
    pub fn components(&self) -> &[PolygonComponent] {
        &self.components
    }

    #[must_use]
    pub fn bounds(&self) -> Option<[f64; 4]> {
        self.components
            .iter()
            .flat_map(|component| component.exterior.iter())
            .fold(None, |bounds, point| {
                Some(match bounds {
                    None => [point.x, point.y, point.x, point.y],
                    Some([min_x, min_y, max_x, max_y]) => [
                        min_x.min(point.x),
                        min_y.min(point.y),
                        max_x.max(point.x),
                        max_y.max(point.y),
                    ],
                })
            })
    }
}
