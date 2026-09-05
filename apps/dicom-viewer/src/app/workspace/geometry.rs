use super::*;

impl WorkspaceRuntime {
    pub(in crate::app) fn begin_handle_drag(&mut self, point: Point2, tolerance: f64) -> bool {
        let tolerance_squared = tolerance * tolerance;
        let mut closest: Option<(f64, Uuid, EditableHandle)> = None;
        for id in self.selection.iter().copied() {
            if self.object_locked(id) {
                continue;
            }
            if let Some(finding) = self.document.finding(id) {
                match finding.geometry() {
                    VectorFindingGeometry::Point(candidate) => update_handle_candidate(
                        &mut closest,
                        point,
                        *candidate,
                        tolerance_squared,
                        id,
                        EditableHandle::PointFinding,
                    ),
                    VectorFindingGeometry::Regions(components) => {
                        for (component_index, component) in components.iter().enumerate() {
                            for (vertex_index, candidate) in component.iter().copied().enumerate() {
                                update_handle_candidate(
                                    &mut closest,
                                    point,
                                    candidate,
                                    tolerance_squared,
                                    id,
                                    EditableHandle::PolygonVertex {
                                        component_index,
                                        vertex_index,
                                    },
                                );
                            }
                        }
                    }
                }
            } else if let Some(segment) = self.document.segment(id) {
                for (primitive_index, primitive) in segment.primitives().iter().enumerate() {
                    let points = match primitive.geometry() {
                        SegmentationPrimitiveGeometry::Polygon { points } => points.as_ref(),
                        SegmentationPrimitiveGeometry::Brush { centerline, .. } => {
                            centerline.as_ref()
                        }
                    };
                    for (point_index, candidate) in points.iter().copied().enumerate() {
                        update_handle_candidate(
                            &mut closest,
                            point,
                            candidate,
                            tolerance_squared,
                            id,
                            EditableHandle::SegmentPrimitivePoint {
                                primitive_index,
                                point_index,
                            },
                        );
                    }
                }
            } else if let Some(measurement) = self.document.measurement(id) {
                for (endpoint_index, candidate) in measurement.endpoints().into_iter().enumerate() {
                    update_handle_candidate(
                        &mut closest,
                        point,
                        candidate,
                        tolerance_squared,
                        id,
                        EditableHandle::MeasurementEndpoint { endpoint_index },
                    );
                }
            }
        }
        let Some((_, object_id, handle)) = closest else {
            return false;
        };
        self.handle_drag = Some(HandleDrag {
            object_id,
            handle,
            before: Arc::clone(&self.document),
        });
        true
    }

    pub(in crate::app) fn update_handle_drag(
        &mut self,
        point: Point2,
        physical_length_mm: impl FnOnce(Point2, Point2) -> Option<f64>,
    ) -> ViewerResult<()> {
        let Some(drag) = &self.handle_drag else {
            return Ok(());
        };
        let object_id = drag.object_id;
        let handle = drag.handle;
        let mut candidate = (*self.document).clone();
        match handle {
            EditableHandle::PointFinding => {
                candidate
                    .replace_vector_geometry(object_id, VectorFindingGeometry::Point(point))?;
            }
            EditableHandle::PolygonVertex {
                component_index,
                vertex_index,
            } => {
                candidate.move_vector_vertex(object_id, component_index, vertex_index, point)?;
            }
            EditableHandle::MeasurementEndpoint { endpoint_index } => {
                let mut endpoints = candidate
                    .measurement(object_id)
                    .ok_or_else(|| {
                        ViewerError::InvalidInput("the dragged measurement no longer exists".into())
                    })?
                    .endpoints();
                endpoints[endpoint_index] = point;
                let length = physical_length_mm(endpoints[0], endpoints[1]);
                candidate.set_measurement_endpoints(object_id, endpoints, length)?;
            }
            EditableHandle::SegmentPrimitivePoint {
                primitive_index,
                point_index,
            } => {
                candidate.move_segment_primitive_point(
                    object_id,
                    primitive_index,
                    point_index,
                    point,
                )?;
            }
        }
        candidate.validate()?;
        self.document = Arc::new(candidate);
        self.invalidate_spatial_index();
        Ok(())
    }

    pub(in crate::app) fn finish_handle_drag(&mut self) -> bool {
        let Some(drag) = self.handle_drag.take() else {
            return false;
        };
        if drag.before.revision() == self.document.revision() {
            return false;
        }
        self.history.record(
            "Move geometry handle",
            drag.before,
            Arc::clone(&self.document),
        );
        true
    }

    pub(in crate::app) fn cancel_handle_drag(&mut self) -> bool {
        let Some(drag) = self.handle_drag.take() else {
            return false;
        };
        self.document = drag.before;
        self.invalidate_spatial_index();
        true
    }

    #[must_use]
    pub(in crate::app) const fn handle_drag_active(&self) -> bool {
        self.handle_drag.is_some()
    }

    pub(in crate::app) fn refresh_spatial_index(&mut self) -> ViewerResult<()> {
        if self.spatial_revision != self.document.revision() {
            self.spatial_index = WorkspaceSpatialIndex::build(&self.document)?;
            self.spatial_revision = self.document.revision();
        }
        Ok(())
    }

    #[must_use]
    pub(in crate::app) fn spatial_index(&self) -> &WorkspaceSpatialIndex {
        &self.spatial_index
    }

    #[must_use]
    pub(in crate::app) fn hit_test(&self, point: Point2, tolerance: f64) -> Option<Uuid> {
        let query = [
            point.x - tolerance,
            point.y - tolerance,
            point.x + tolerance,
            point.y + tolerance,
        ];
        self.spatial_index
            .query(query, 256)
            .into_iter()
            .filter_map(|id| {
                object_distance(self.document(), id, point).map(|distance| (id, distance))
            })
            .filter(|(_, distance)| *distance <= tolerance)
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(id, _)| id)
    }

    #[must_use]
    pub(in crate::app) fn object_bounds(&self, id: Uuid) -> Option<[f64; 4]> {
        self.spatial_index.bounds(id)
    }

    pub(in crate::app) fn invalidate_spatial_index(&mut self) {
        self.spatial_revision = u64::MAX;
    }
}

fn object_distance(document: &WorkspaceDocument, id: Uuid, point: Point2) -> Option<f64> {
    if let Some(finding) = document.finding(id) {
        return Some(match finding.geometry() {
            dicom_viewer_core::VectorFindingGeometry::Point(candidate) => {
                (candidate.x - point.x).hypot(candidate.y - point.y)
            }
            dicom_viewer_core::VectorFindingGeometry::Regions(components) => components
                .iter()
                .map(|component| polygon_distance(component, point))
                .fold(f64::INFINITY, f64::min),
        });
    }
    if let Some(segment) = document.segment(id) {
        let geometry = document.composite_segment(segment.object_id()).ok()?;
        return geometry
            .components()
            .iter()
            .map(|component| {
                let inside = dicom_viewer_core::polygon_contains_point(component.exterior(), point)
                    && !component
                        .holes()
                        .iter()
                        .any(|hole| dicom_viewer_core::polygon_contains_point(hole, point));
                if inside {
                    0.0
                } else {
                    std::iter::once(component.exterior())
                        .chain(component.holes().iter().map(Vec::as_slice))
                        .map(|ring| ring_edge_distance(ring, point))
                        .fold(f64::INFINITY, f64::min)
                }
            })
            .min_by(f64::total_cmp);
    }
    let measurement = document.measurement(id)?;
    let endpoints = measurement.endpoints();
    Some(segment_distance(endpoints[0], endpoints[1], point))
}

fn update_handle_candidate(
    closest: &mut Option<(f64, Uuid, EditableHandle)>,
    target: Point2,
    candidate: Point2,
    tolerance_squared: f64,
    object_id: Uuid,
    handle: EditableHandle,
) {
    let dx = candidate.x - target.x;
    let dy = candidate.y - target.y;
    let distance_squared = dx * dx + dy * dy;
    if distance_squared > tolerance_squared
        || closest
            .as_ref()
            .is_some_and(|(best, _, _)| *best <= distance_squared)
    {
        return;
    }
    *closest = Some((distance_squared, object_id, handle));
}

fn polygon_distance(polygon: &[Point2], point: Point2) -> f64 {
    if dicom_viewer_core::polygon_contains_point(polygon, point) {
        0.0
    } else {
        ring_edge_distance(polygon, point)
    }
}

fn ring_edge_distance(points: &[Point2], point: Point2) -> f64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(start, end)| segment_distance(*start, *end, point))
        .fold(f64::INFINITY, f64::min)
}

fn segment_distance(start: Point2, end: Point2, point: Point2) -> f64 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_squared = dx * dx + dy * dy;
    if length_squared <= f64::EPSILON {
        return (point.x - start.x).hypot(point.y - start.y);
    }
    let fraction =
        (((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared).clamp(0.0, 1.0);
    let closest = Point2::new(start.x + fraction * dx, start.y + fraction * dy);
    (point.x - closest.x).hypot(point.y - closest.y)
}
