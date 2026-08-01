use std::fs;
use std::io::Write;
use std::path::Path;

use eframe::egui::{self, Pos2, Rect, Shape, Stroke, Vec2};
use serde_json::{json, Value};

use super::camera::{CameraView, MIN_ZOOM};
use super::theme;

const MIN_POLYGON_AREA: f64 = 0.5;
const VERTEX_RADIUS: f32 = 3.5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::app) enum AnnotationMode {
    #[default]
    Tumor,
    Hole,
}

impl AnnotationMode {
    pub(in crate::app) fn label(self) -> &'static str {
        match self {
            Self::Tumor => "Tumor",
            Self::Hole => "Exclude",
        }
    }
}

#[derive(Debug, Clone)]
struct CompletedRing {
    mode: AnnotationMode,
    points: Vec<Vec2>,
    owner: Option<usize>,
}

#[derive(Debug, Default)]
pub(super) struct AnnotationState {
    pub(super) active: bool,
    pub(in crate::app) mode: AnnotationMode,
    pub(super) current: Vec<Vec2>,
    completed: Vec<CompletedRing>,
    dirty: bool,
}

impl AnnotationState {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn add_vertex(&mut self, point: Vec2) {
        if self
            .current
            .last()
            .is_none_or(|last| (*last - point).length_sq() > f32::EPSILON)
        {
            self.current.push(point);
            self.dirty = true;
        }
    }

    pub(super) fn set_mode(&mut self, mode: AnnotationMode) -> bool {
        if self.mode == mode {
            return false;
        }
        let discarded = !self.current.is_empty();
        self.current.clear();
        self.dirty |= discarded;
        self.mode = mode;
        discarded
    }

    pub(super) fn close_current(&mut self) -> Result<(), AnnotationError> {
        validate_simple_ring(&self.current)?;

        let owner = match self.mode {
            AnnotationMode::Tumor => {
                if self
                    .completed
                    .iter()
                    .filter(|ring| ring.mode == AnnotationMode::Tumor)
                    .any(|ring| rings_overlap(&ring.points, &self.current))
                {
                    return Err(AnnotationError::OverlappingTumor);
                }
                None
            }
            AnnotationMode::Hole => Some(
                self.containing_tumor(&self.current)
                    .ok_or(AnnotationError::HoleOutsideTumor)?,
            ),
        };

        if let Some(owner) = owner {
            if self.completed.iter().any(|ring| {
                ring.mode == AnnotationMode::Hole
                    && ring.owner == Some(owner)
                    && rings_overlap(&ring.points, &self.current)
            }) {
                return Err(AnnotationError::OverlappingHole);
            }
        }

        self.completed.push(CompletedRing {
            mode: self.mode,
            points: std::mem::take(&mut self.current),
            owner,
        });
        Ok(())
    }

    pub(super) fn undo(&mut self) -> bool {
        let changed = if self.current.pop().is_some() {
            true
        } else {
            self.completed.pop().is_some()
        };
        if changed {
            self.dirty = true;
        }
        changed
    }

    pub(super) fn completed_tumor_count(&self) -> usize {
        self.completed
            .iter()
            .filter(|ring| ring.mode == AnnotationMode::Tumor)
            .count()
    }

    pub(super) fn has_current_vertices(&self) -> bool {
        !self.current.is_empty()
    }

    pub(super) fn has_unsaved_work(&self) -> bool {
        self.dirty && (self.has_current_vertices() || !self.completed.is_empty())
    }

    pub(super) fn mark_saved(&mut self) {
        self.dirty = false;
    }

    pub(super) fn geojson_value(&self) -> Result<Value, AnnotationError> {
        if self.completed_tumor_count() == 0 {
            return Err(AnnotationError::NoTumor);
        }

        let mut features = Vec::with_capacity(self.completed_tumor_count());
        let mut fragment_number = 0_usize;
        for (ring_index, outer) in self.completed.iter().enumerate() {
            if outer.mode != AnnotationMode::Tumor {
                continue;
            }
            fragment_number += 1;
            let fragment_id = format!("F{fragment_number:03}");
            let mut coordinates = vec![closed_coordinates(&outer.points, true)];
            coordinates.extend(
                self.completed
                    .iter()
                    .filter(|ring| {
                        ring.mode == AnnotationMode::Hole && ring.owner == Some(ring_index)
                    })
                    .map(|ring| closed_coordinates(&ring.points, false)),
            );
            features.push(json!({
                "type": "Feature",
                "properties": {
                    "objectType": "annotation",
                    "name": fragment_id,
                    "fragment_id": fragment_id,
                    "coordinate_space": "level-0_pixels",
                    "classification": {
                        "name": "viable_tumor"
                    }
                },
                "geometry": {
                    "type": "Polygon",
                    "coordinates": coordinates
                }
            }));
        }

        Ok(json!({
            "type": "FeatureCollection",
            "features": features
        }))
    }

    pub(super) fn save_geojson(&self, path: &Path) -> Result<(), AnnotationError> {
        let bytes = serde_json::to_vec_pretty(&self.geojson_value()?)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file_mut().sync_all()?;
        temporary.persist(path).map_err(|error| error.error)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }

    fn containing_tumor(&self, hole: &[Vec2]) -> Option<usize> {
        self.completed
            .iter()
            .enumerate()
            .filter(|(_, ring)| ring.mode == AnnotationMode::Tumor)
            .filter(|(_, ring)| ring_strictly_contains(&ring.points, hole))
            .min_by(|(_, a), (_, b)| {
                signed_area(&a.points)
                    .abs()
                    .total_cmp(&signed_area(&b.points).abs())
            })
            .map(|(index, _)| index)
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AnnotationError {
    #[error("A polygon needs at least three vertices.")]
    TooFewVertices,
    #[error("The polygon has zero area.")]
    ZeroArea,
    #[error("The polygon crosses itself.")]
    SelfIntersection,
    #[error("An exclusion must be fully inside one viable-tumor polygon.")]
    HoleOutsideTumor,
    #[error("Exclusion polygons cannot overlap.")]
    OverlappingHole,
    #[error("Viable-tumor polygons cannot overlap.")]
    OverlappingTumor,
    #[error("Draw at least one viable-tumor polygon first.")]
    NoTumor,
    #[error("Could not serialize GeoJSON: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("Could not write GeoJSON: {0}")]
    Write(#[from] std::io::Error),
}

pub(super) fn draw_annotation_overlay(
    painter: &egui::Painter,
    rect: Rect,
    annotations: &AnnotationState,
    hover_base: Option<Vec2>,
    view: CameraView,
) {
    for ring in &annotations.completed {
        let points = screen_points(rect, &ring.points, view);
        if points.len() < 3 {
            continue;
        }
        let stroke = match ring.mode {
            AnnotationMode::Tumor => Stroke::new(2.0, theme::GREEN),
            AnnotationMode::Hole => Stroke::new(2.0, theme::AMBER_BRIGHT),
        };
        painter.add(Shape::closed_line(points, stroke));
    }

    if annotations.current.is_empty() {
        return;
    }
    let mut points = screen_points(rect, &annotations.current, view);
    if let Some(hover) = hover_base {
        points.push(base_to_screen(rect, hover, view));
    }
    let color = match annotations.mode {
        AnnotationMode::Tumor => theme::GREEN,
        AnnotationMode::Hole => theme::AMBER_BRIGHT,
    };
    if points.len() >= 2 {
        painter.add(Shape::line(points, Stroke::new(2.0, color)));
    }
    for point in &annotations.current {
        let screen = base_to_screen(rect, *point, view);
        painter.circle_filled(screen, VERTEX_RADIUS + 1.5, theme::CANVAS_EDGE);
        painter.circle_filled(screen, VERTEX_RADIUS, color);
    }
}

fn screen_points(rect: Rect, points: &[Vec2], view: CameraView) -> Vec<Pos2> {
    points
        .iter()
        .map(|point| base_to_screen(rect, *point, view))
        .collect()
}

fn base_to_screen(rect: Rect, base: Vec2, view: CameraView) -> Pos2 {
    rect.center() + (base - view.center_base) * view.zoom.max(MIN_ZOOM)
}

fn validate_simple_ring(points: &[Vec2]) -> Result<(), AnnotationError> {
    if points.len() < 3 {
        return Err(AnnotationError::TooFewVertices);
    }
    if signed_area(points).abs() < MIN_POLYGON_AREA {
        return Err(AnnotationError::ZeroArea);
    }
    if ring_self_intersects(points) {
        return Err(AnnotationError::SelfIntersection);
    }
    Ok(())
}

fn signed_area(points: &[Vec2]) -> f64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| f64::from(a.x) * f64::from(b.y) - f64::from(b.x) * f64::from(a.y))
        .sum::<f64>()
        * 0.5
}

fn ring_self_intersects(points: &[Vec2]) -> bool {
    let edge_count = points.len();
    for first in 0..edge_count {
        let first_next = (first + 1) % edge_count;
        for second in (first + 1)..edge_count {
            let second_next = (second + 1) % edge_count;
            if first == second
                || first_next == second
                || second_next == first
                || (first == 0 && second_next == 0)
            {
                continue;
            }
            if segments_intersect(
                points[first],
                points[first_next],
                points[second],
                points[second_next],
            ) {
                return true;
            }
        }
    }
    false
}

fn rings_overlap(a: &[Vec2], b: &[Vec2]) -> bool {
    edges(a).any(|(a1, a2)| edges(b).any(|(b1, b2)| segments_intersect(a1, a2, b1, b2)))
        || point_in_ring(a, b[0], false)
        || point_in_ring(b, a[0], false)
}

fn ring_strictly_contains(outer: &[Vec2], inner: &[Vec2]) -> bool {
    inner.iter().all(|point| point_in_ring(outer, *point, true))
        && !edges(outer)
            .any(|(a1, a2)| edges(inner).any(|(b1, b2)| segments_intersect(a1, a2, b1, b2)))
}

fn edges(points: &[Vec2]) -> impl Iterator<Item = (Vec2, Vec2)> + '_ {
    points
        .iter()
        .copied()
        .zip(points.iter().copied().cycle().skip(1))
        .take(points.len())
}

fn point_in_ring(ring: &[Vec2], point: Vec2, strict: bool) -> bool {
    if edges(ring).any(|(a, b)| point_on_segment(point, a, b)) {
        return !strict;
    }
    let mut inside = false;
    for (a, b) in edges(ring) {
        let crosses_y = (a.y > point.y) != (b.y > point.y);
        if crosses_y {
            let x = (b.x - a.x) * (point.y - a.y) / (b.y - a.y) + a.x;
            if point.x < x {
                inside = !inside;
            }
        }
    }
    inside
}

fn point_on_segment(point: Vec2, a: Vec2, b: Vec2) -> bool {
    orientation(a, b, point).abs() <= f64::EPSILON
        && f64::from(point.x) >= f64::from(a.x.min(b.x))
        && f64::from(point.x) <= f64::from(a.x.max(b.x))
        && f64::from(point.y) >= f64::from(a.y.min(b.y))
        && f64::from(point.y) <= f64::from(a.y.max(b.y))
}

fn segments_intersect(a1: Vec2, a2: Vec2, b1: Vec2, b2: Vec2) -> bool {
    let o1 = orientation(a1, a2, b1);
    let o2 = orientation(a1, a2, b2);
    let o3 = orientation(b1, b2, a1);
    let o4 = orientation(b1, b2, a2);
    if ((o1 > 0.0 && o2 < 0.0) || (o1 < 0.0 && o2 > 0.0))
        && ((o3 > 0.0 && o4 < 0.0) || (o3 < 0.0 && o4 > 0.0))
    {
        return true;
    }
    (o1.abs() <= f64::EPSILON && point_on_segment(b1, a1, a2))
        || (o2.abs() <= f64::EPSILON && point_on_segment(b2, a1, a2))
        || (o3.abs() <= f64::EPSILON && point_on_segment(a1, b1, b2))
        || (o4.abs() <= f64::EPSILON && point_on_segment(a2, b1, b2))
}

fn orientation(a: Vec2, b: Vec2, c: Vec2) -> f64 {
    (f64::from(b.x) - f64::from(a.x)) * (f64::from(c.y) - f64::from(a.y))
        - (f64::from(b.y) - f64::from(a.y)) * (f64::from(c.x) - f64::from(a.x))
}

fn closed_coordinates(points: &[Vec2], counterclockwise: bool) -> Vec<[f64; 2]> {
    let mut ordered = points.to_vec();
    let is_counterclockwise = signed_area(&ordered) > 0.0;
    if is_counterclockwise != counterclockwise {
        ordered.reverse();
    }
    ordered.push(ordered[0]);
    ordered
        .into_iter()
        .map(|point| [f64::from(point.x), f64::from(point.y)])
        .collect()
}

#[cfg(test)]
mod tests {
    use eframe::egui::vec2;

    use super::{AnnotationMode, AnnotationState};

    #[test]
    fn closes_valid_tumor_ring_and_rejects_short_or_degenerate_rings() {
        let mut annotations = AnnotationState::default();
        annotations.add_vertex(vec2(0.0, 0.0));
        annotations.add_vertex(vec2(10.0, 0.0));
        assert_eq!(
            annotations.close_current().unwrap_err().to_string(),
            "A polygon needs at least three vertices."
        );

        annotations.add_vertex(vec2(20.0, 0.0));
        assert_eq!(
            annotations.close_current().unwrap_err().to_string(),
            "The polygon has zero area."
        );

        annotations.current.clear();
        annotations.add_vertex(vec2(0.0, 0.0));
        annotations.add_vertex(vec2(10.0, 0.0));
        annotations.add_vertex(vec2(10.0, 10.0));
        annotations.close_current().unwrap();

        assert_eq!(annotations.completed_tumor_count(), 1);
        assert!(annotations.current.is_empty());
    }

    #[test]
    fn hole_must_be_inside_a_tumor_component() {
        let mut annotations = AnnotationState::default();
        annotations.set_mode(AnnotationMode::Hole);
        annotations.add_vertex(vec2(1.0, 1.0));
        annotations.add_vertex(vec2(2.0, 1.0));
        annotations.add_vertex(vec2(2.0, 2.0));

        assert_eq!(
            annotations.close_current().unwrap_err().to_string(),
            "An exclusion must be fully inside one viable-tumor polygon."
        );
        assert_eq!(annotations.completed_tumor_count(), 0);
    }

    #[test]
    fn export_is_level_zero_geojson_with_exact_class_and_closed_rings() {
        let mut annotations = AnnotationState::default();
        for point in [
            vec2(0.0, 0.0),
            vec2(20.0, 0.0),
            vec2(20.0, 20.0),
            vec2(0.0, 20.0),
        ] {
            annotations.add_vertex(point);
        }
        annotations.close_current().unwrap();

        annotations.mode = AnnotationMode::Hole;
        for point in [
            vec2(5.0, 5.0),
            vec2(10.0, 5.0),
            vec2(10.0, 10.0),
            vec2(5.0, 10.0),
        ] {
            annotations.add_vertex(point);
        }
        annotations.close_current().unwrap();

        let value = annotations.geojson_value().unwrap();
        assert_eq!(value["type"], "FeatureCollection");
        let features = value["features"].as_array().unwrap();
        assert_eq!(features.len(), 1);
        assert_eq!(
            features[0]["properties"]["classification"]["name"],
            "viable_tumor"
        );
        assert_eq!(features[0]["properties"]["fragment_id"], "F001");
        let rings = features[0]["geometry"]["coordinates"].as_array().unwrap();
        assert_eq!(rings.len(), 2);
        for ring in rings {
            let points = ring.as_array().unwrap();
            assert_eq!(points.first(), points.last());
        }
    }

    #[test]
    fn undo_removes_current_vertices_before_completed_rings() {
        let mut annotations = AnnotationState::default();
        for point in [vec2(0.0, 0.0), vec2(5.0, 0.0), vec2(5.0, 5.0)] {
            annotations.add_vertex(point);
        }
        annotations.close_current().unwrap();
        annotations.add_vertex(vec2(9.0, 9.0));

        assert!(annotations.undo());
        assert_eq!(annotations.completed_tumor_count(), 1);
        assert!(annotations.current.is_empty());
        assert!(annotations.undo());
        assert_eq!(annotations.completed_tumor_count(), 0);
        assert!(!annotations.undo());
    }

    #[test]
    fn saved_annotations_become_dirty_again_after_an_edit() {
        let mut annotations = AnnotationState::default();
        annotations.add_vertex(vec2(0.0, 0.0));
        assert!(annotations.has_unsaved_work());

        annotations.mark_saved();
        assert!(!annotations.has_unsaved_work());

        annotations.add_vertex(vec2(5.0, 0.0));
        assert!(annotations.has_unsaved_work());
    }

    #[cfg(unix)]
    #[test]
    fn export_atomically_replaces_a_destination_symlink_without_overwriting_its_target() {
        use std::os::unix::fs::symlink;

        let mut annotations = AnnotationState::default();
        for point in [vec2(0.0, 0.0), vec2(10.0, 0.0), vec2(10.0, 10.0)] {
            annotations.add_vertex(point);
        }
        annotations.close_current().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("existing-target.json");
        let export = dir.path().join("annotations.geojson");
        std::fs::write(&target, b"preserve me").unwrap();
        symlink(&target, &export).unwrap();

        annotations.save_geojson(&export).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"preserve me");
        assert!(std::fs::symlink_metadata(&export)
            .unwrap()
            .file_type()
            .is_file());
        let exported: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&export).unwrap()).unwrap();
        assert_eq!(exported["type"], "FeatureCollection");
    }
}
