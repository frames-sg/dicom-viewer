use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dicom_viewer_core::{
    CompositeSegmentGeometry, Point2, SegmentationPrimitive, VectorFindingGeometry, ViewerError,
    WorkspaceDocument, WorkspaceLinearMeasurement, WorkspaceObjectRef,
};
use rstar::{RTree, RTreeObject, AABB};
use uuid::Uuid;

#[derive(Debug, Clone)]
struct IndexedObject {
    id: Uuid,
    envelope: AABB<[f64; 2]>,
    point: Option<ViewportPoint>,
}

#[derive(Debug, Clone)]
pub(super) struct ViewportPoint {
    id: Uuid,
    layer_id: Uuid,
    class_id: String,
    point: Point2,
}

impl ViewportPoint {
    pub(super) const fn id(&self) -> Uuid {
        self.id
    }

    pub(super) const fn layer_id(&self) -> Uuid {
        self.layer_id
    }

    pub(super) fn class_id(&self) -> &str {
        &self.class_id
    }

    pub(super) const fn point(&self) -> Point2 {
        self.point
    }
}

#[derive(Debug, Default)]
pub(super) struct SpatialViewportQuery<'a> {
    detailed_ids: Vec<Uuid>,
    points: Vec<&'a ViewportPoint>,
}

#[derive(Debug, Clone)]
pub(super) enum SpatialRenderGeometry {
    Vector(VectorFindingGeometry),
    Segment {
        composite: CompositeSegmentGeometry,
        primitives: Arc<[SegmentationPrimitive]>,
    },
    Measurement([Point2; 2]),
}

#[derive(Debug, Clone)]
pub(super) struct SpatialRenderObject {
    bounds: [f64; 4],
    layer_id: Option<Uuid>,
    class_id: String,
    geometry: SpatialRenderGeometry,
}

impl SpatialRenderObject {
    pub(super) const fn layer_id(&self) -> Option<Uuid> {
        self.layer_id
    }

    pub(super) fn class_id(&self) -> &str {
        &self.class_id
    }

    pub(super) const fn geometry(&self) -> &SpatialRenderGeometry {
        &self.geometry
    }

    pub(super) fn vertex_count(&self) -> usize {
        match &self.geometry {
            SpatialRenderGeometry::Vector(geometry) => geometry.coordinate_count(),
            SpatialRenderGeometry::Segment { composite, .. } => composite
                .components()
                .iter()
                .map(|component| {
                    component.exterior().len()
                        + component.holes().iter().map(Vec::len).sum::<usize>()
                })
                .sum(),
            SpatialRenderGeometry::Measurement(_) => 2,
        }
    }
}

impl SpatialViewportQuery<'_> {
    pub(super) fn detailed_ids(&self) -> &[Uuid] {
        &self.detailed_ids
    }

    pub(super) fn points(&self) -> &[&ViewportPoint] {
        &self.points
    }
}

impl PartialEq for IndexedObject {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl RTreeObject for IndexedObject {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.envelope
    }
}

#[derive(Debug, Default)]
pub(in crate::app) struct WorkspaceSpatialIndex {
    tree: RTree<IndexedObject>,
    render_objects: HashMap<Uuid, SpatialRenderObject>,
}

impl WorkspaceSpatialIndex {
    pub(super) fn build(document: &WorkspaceDocument) -> Result<Self, ViewerError> {
        let mut objects = Vec::with_capacity(document.object_count());
        let mut render_objects = HashMap::with_capacity(document.object_count());
        let records = document
            .vector_layers()
            .iter()
            .flat_map(|layer| {
                layer
                    .findings()
                    .iter()
                    .map(move |finding| (Some(layer.id()), WorkspaceObjectRef::Vector(finding)))
            })
            .chain(document.segmentation_layers().iter().flat_map(|layer| {
                layer
                    .segments()
                    .iter()
                    .map(move |segment| (Some(layer.id()), WorkspaceObjectRef::Segment(segment)))
            }))
            .chain(
                document
                    .measurements()
                    .iter()
                    .map(|measurement| (None, WorkspaceObjectRef::Measurement(measurement))),
            );
        for (layer, object) in records {
            if let Some((indexed, render)) = prepare_object(document, layer, object)? {
                objects.push(indexed);
                render_objects.insert(object.object_id(), render);
            }
        }
        Ok(Self {
            tree: RTree::bulk_load(objects),
            render_objects,
        })
    }

    // A handle edit cannot move an object between layers. Prepare everything
    // before touching either index so a failed geometry operation is atomic.
    pub(super) fn update_object(
        &mut self,
        document: &WorkspaceDocument,
        id: Uuid,
    ) -> Result<(), ViewerError> {
        let old = self.render_objects.get(&id).ok_or_else(|| {
            ViewerError::InvalidInput("dragged object is absent from the spatial index".into())
        })?;
        let object = document.object(id).ok_or_else(|| {
            ViewerError::InvalidInput("dragged object is absent from the document".into())
        })?;
        let replacement = prepare_object(document, old.layer_id, object)?;
        let old_index = indexed(id, old.bounds, None);
        self.tree.remove(&old_index);
        self.render_objects.remove(&id);
        if let Some((indexed, render)) = replacement {
            self.tree.insert(indexed);
            self.render_objects.insert(id, render);
        }
        Ok(())
    }

    #[must_use]
    pub(super) fn query(&self, bounds: [f64; 4], limit: usize) -> Vec<Uuid> {
        let envelope = AABB::from_corners([bounds[0], bounds[1]], [bounds[2], bounds[3]]);
        let mut ids = self
            .tree
            .locate_in_envelope_intersecting(&envelope)
            .map(|object| object.id)
            .take(limit)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    #[must_use]
    pub(super) fn query_for_overlay(
        &self,
        bounds: [f64; 4],
        detailed_limit: usize,
        selected: &HashSet<Uuid>,
    ) -> SpatialViewportQuery<'_> {
        let envelope = AABB::from_corners([bounds[0], bounds[1]], [bounds[2], bounds[3]]);
        let mut result = SpatialViewportQuery::default();
        let mut unselected_detail_count = 0usize;
        for object in self.tree.locate_in_envelope_intersecting(&envelope) {
            if selected.contains(&object.id) {
                result.detailed_ids.push(object.id);
            } else if let Some(point) = &object.point {
                result.points.push(point);
            } else if unselected_detail_count < detailed_limit {
                result.detailed_ids.push(object.id);
                unselected_detail_count += 1;
            }
        }
        result.detailed_ids.sort_unstable();
        result.detailed_ids.dedup();
        result.points.sort_unstable_by_key(|point| point.id());
        result
    }

    #[must_use]
    pub(super) fn bounds(&self, id: Uuid) -> Option<[f64; 4]> {
        self.render_objects.get(&id).map(|object| object.bounds)
    }

    #[must_use]
    pub(super) fn render_object(&self, id: Uuid) -> Option<&SpatialRenderObject> {
        self.render_objects.get(&id)
    }
}

fn indexed(id: Uuid, bounds: [f64; 4], point: Option<ViewportPoint>) -> IndexedObject {
    IndexedObject {
        id,
        envelope: AABB::from_corners([bounds[0], bounds[1]], [bounds[2], bounds[3]]),
        point,
    }
}

fn measurement_bounds(measurement: &WorkspaceLinearMeasurement) -> [f64; 4] {
    let endpoints = measurement.endpoints();
    [
        endpoints[0].x.min(endpoints[1].x),
        endpoints[0].y.min(endpoints[1].y),
        endpoints[0].x.max(endpoints[1].x),
        endpoints[0].y.max(endpoints[1].y),
    ]
}

fn bounds(points: impl Iterator<Item = [f64; 2]>) -> Option<[f64; 4]> {
    points.fold(None, |bounds, [x, y]| {
        Some(match bounds {
            None => [x, y, x, y],
            Some([min_x, min_y, max_x, max_y]) => {
                [min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y)]
            }
        })
    })
}

fn prepare_object(
    document: &WorkspaceDocument,
    layer_id: Option<Uuid>,
    object: WorkspaceObjectRef<'_>,
) -> Result<Option<(IndexedObject, SpatialRenderObject)>, ViewerError> {
    let id = object.object_id();
    let class_id = object.class_id().to_owned();
    let (bounds, point, geometry) = match object {
        WorkspaceObjectRef::Vector(finding) => {
            let (bounds, point) =
                match finding.geometry() {
                    VectorFindingGeometry::Point(point) => (
                        [point.x, point.y, point.x, point.y],
                        Some(ViewportPoint {
                            id,
                            layer_id: layer_id.expect("vector index records have a layer"),
                            class_id: class_id.clone(),
                            point: *point,
                        }),
                    ),
                    VectorFindingGeometry::Regions(components) => (
                        bounds(components.iter().flat_map(|component| {
                            component.iter().map(|point| [point.x, point.y])
                        }))
                        .ok_or_else(|| {
                            ViewerError::InvalidInput("vector finding has no coordinates".into())
                        })?,
                        None,
                    ),
                };
            (
                bounds,
                point,
                SpatialRenderGeometry::Vector(finding.geometry().clone()),
            )
        }
        WorkspaceObjectRef::Segment(segment) => {
            let composite = document.composite_segment(id)?;
            let Some(bounds) = composite.bounds() else {
                return Ok(None);
            };
            (
                bounds,
                None,
                SpatialRenderGeometry::Segment {
                    composite,
                    primitives: Arc::from(segment.primitives()),
                },
            )
        }
        WorkspaceObjectRef::Measurement(measurement) => (
            measurement_bounds(measurement),
            None,
            SpatialRenderGeometry::Measurement(measurement.endpoints()),
        ),
    };
    Ok(Some((
        indexed(id, bounds, point),
        SpatialRenderObject {
            bounds,
            layer_id,
            class_id,
            geometry,
        },
    )))
}
