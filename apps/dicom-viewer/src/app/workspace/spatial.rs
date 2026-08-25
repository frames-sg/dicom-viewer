use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dicom_viewer_core::{
    CompositeSegmentGeometry, Point2, SegmentationPrimitive, VectorFindingGeometry, ViewerError,
    WorkspaceDocument, WorkspaceLinearMeasurement,
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
pub(super) struct SpatialViewportQuery {
    detailed_ids: Vec<Uuid>,
    points: Vec<ViewportPoint>,
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

impl SpatialViewportQuery {
    pub(super) fn detailed_ids(&self) -> &[Uuid] {
        &self.detailed_ids
    }

    pub(super) fn points(&self) -> &[ViewportPoint] {
        &self.points
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
        for layer in document.vector_layers() {
            for finding in layer.findings() {
                let (bounds, point) = match finding.geometry() {
                    VectorFindingGeometry::Point(point) => (
                        [point.x, point.y, point.x, point.y],
                        Some(ViewportPoint {
                            id: finding.object_id(),
                            layer_id: layer.id(),
                            class_id: finding.class_id().to_owned(),
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
                objects.push(indexed(finding.object_id(), bounds, point));
                render_objects.insert(
                    finding.object_id(),
                    SpatialRenderObject {
                        layer_id: Some(layer.id()),
                        class_id: finding.class_id().to_owned(),
                        geometry: SpatialRenderGeometry::Vector(finding.geometry().clone()),
                    },
                );
            }
        }
        for layer in document.segmentation_layers() {
            for segment in layer.segments() {
                let composite = document.composite_segment(segment.object_id())?;
                if let Some(bounds) = composite.bounds() {
                    objects.push(indexed(segment.object_id(), bounds, None));
                    render_objects.insert(
                        segment.object_id(),
                        SpatialRenderObject {
                            layer_id: Some(layer.id()),
                            class_id: segment.class_id().to_owned(),
                            geometry: SpatialRenderGeometry::Segment {
                                composite,
                                primitives: Arc::from(segment.primitives()),
                            },
                        },
                    );
                }
            }
        }
        for measurement in document.measurements() {
            objects.push(indexed(
                measurement.object_id(),
                measurement_bounds(measurement),
                None,
            ));
            render_objects.insert(
                measurement.object_id(),
                SpatialRenderObject {
                    layer_id: None,
                    class_id: measurement.class_id().to_owned(),
                    geometry: SpatialRenderGeometry::Measurement(measurement.endpoints()),
                },
            );
        }
        Ok(Self {
            tree: RTree::bulk_load(objects),
            render_objects,
        })
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
    ) -> SpatialViewportQuery {
        let envelope = AABB::from_corners([bounds[0], bounds[1]], [bounds[2], bounds[3]]);
        let mut result = SpatialViewportQuery::default();
        let mut unselected_detail_count = 0usize;
        for object in self.tree.locate_in_envelope_intersecting(&envelope) {
            if selected.contains(&object.id) {
                result.detailed_ids.push(object.id);
            } else if let Some(point) = &object.point {
                result.points.push(point.clone());
            } else if unselected_detail_count < detailed_limit {
                result.detailed_ids.push(object.id);
                unselected_detail_count += 1;
            }
        }
        result.detailed_ids.sort_unstable();
        result.detailed_ids.dedup();
        result.points.sort_unstable_by_key(ViewportPoint::id);
        result
    }

    #[must_use]
    pub(super) fn bounds(&self, id: Uuid) -> Option<[f64; 4]> {
        self.tree
            .iter()
            .find(|object| object.id == id)
            .map(|object| {
                let lower = object.envelope.lower();
                let upper = object.envelope.upper();
                [lower[0], lower[1], upper[0], upper[1]]
            })
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
