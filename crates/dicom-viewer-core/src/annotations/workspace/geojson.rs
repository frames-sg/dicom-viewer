use std::cmp::Ordering;

use serde::Serialize;
use serde_json::{json, Value};

use crate::{polygon_signed_area, Result, ViewerError};

use super::document::WorkspaceDocument;
use super::model::{
    ControlledFindingSite, PolygonComponent, SourceFrameContext, VectorFindingGeometry,
    WorkspaceObjectProvenance,
};

const GEOJSON_SCHEMA_VERSION: &str = "frames-pathology-geojson-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGeoJsonExport {
    bytes: Vec<u8>,
    excluded_measurement_count: usize,
}

impl WorkspaceGeoJsonExport {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn excluded_measurement_count(&self) -> usize {
        self.excluded_measurement_count
    }
}

#[derive(Serialize)]
struct FeatureCollection<'a> {
    #[serde(rename = "type")]
    object_type: &'static str,
    frames_pathology: Metadata<'a>,
    features: Vec<Feature<'a>>,
}

#[derive(Serialize)]
struct Metadata<'a> {
    schema_version: &'static str,
    coordinate_space: CoordinateSpace,
    source_identity: &'a crate::ViewerSourceIdentity,
    scheme: SchemeMetadata<'a>,
}

#[derive(Serialize)]
struct CoordinateSpace {
    name: &'static str,
    origin: &'static str,
    x_direction: &'static str,
    y_direction: &'static str,
}

#[derive(Serialize)]
struct SchemeMetadata<'a> {
    scheme_id: &'a str,
    scheme_version: u32,
    content_digest: &'a str,
}

#[derive(Serialize)]
struct Feature<'a> {
    #[serde(rename = "type")]
    object_type: &'static str,
    id: String,
    properties: FeatureProperties<'a>,
    geometry: Geometry,
}

#[derive(Serialize)]
struct FeatureProperties<'a> {
    object_id: String,
    ordinal: u64,
    tracking_id: &'a str,
    tracking_uid: &'a str,
    class_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    finding_site: Option<&'a ControlledFindingSite>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<&'a str>,
    source_layer_id: String,
    source_layer_name: &'a str,
    representation: &'static str,
    provenance: &'a WorkspaceObjectProvenance,
    source_frame: &'a SourceFrameContext,
}

#[derive(Serialize)]
struct Geometry {
    #[serde(rename = "type")]
    geometry_type: &'static str,
    coordinates: Value,
}

impl WorkspaceDocument {
    pub fn export_pathology_geojson(&self) -> Result<WorkspaceGeoJsonExport> {
        let mut features = Vec::new();
        for layer in self.vector_layers() {
            for finding in layer.findings() {
                let geometry = match finding.geometry() {
                    VectorFindingGeometry::Point(point) => Geometry {
                        geometry_type: "Point",
                        coordinates: json!([point.x, point.y]),
                    },
                    VectorFindingGeometry::Regions(components) => polygon_geometry(
                        components
                            .iter()
                            .map(|component| PolygonComponent::new(component.to_vec(), Vec::new()))
                            .collect(),
                    )?,
                };
                features.push((
                    finding.ordinal(),
                    Feature {
                        object_type: "Feature",
                        id: finding.object_id().to_string(),
                        properties: FeatureProperties {
                            object_id: finding.object_id().to_string(),
                            ordinal: finding.ordinal(),
                            tracking_id: finding.tracking().id(),
                            tracking_uid: finding.tracking().uid(),
                            class_id: finding.class_id(),
                            finding_site: finding.finding_site(),
                            name: finding.name(),
                            comment: finding.comment(),
                            source_layer_id: layer.id().to_string(),
                            source_layer_name: layer.name(),
                            representation: "VECTOR_FINDING",
                            provenance: finding.provenance(),
                            source_frame: finding.source_frame(),
                        },
                        geometry,
                    },
                ));
            }
        }
        for layer in self.segmentation_layers() {
            for segment in layer.segments() {
                let geometry = polygon_geometry(
                    self.composite_segment(segment.object_id())?
                        .components()
                        .to_vec(),
                )?;
                features.push((
                    segment.ordinal(),
                    Feature {
                        object_type: "Feature",
                        id: segment.object_id().to_string(),
                        properties: FeatureProperties {
                            object_id: segment.object_id().to_string(),
                            ordinal: segment.ordinal(),
                            tracking_id: segment.tracking().id(),
                            tracking_uid: segment.tracking().uid(),
                            class_id: segment.class_id(),
                            finding_site: segment.finding_site(),
                            name: segment.name(),
                            comment: segment.comment(),
                            source_layer_id: layer.id().to_string(),
                            source_layer_name: layer.name(),
                            representation: "SEGMENTATION_SEGMENT",
                            provenance: segment.provenance(),
                            source_frame: segment.source_frame(),
                        },
                        geometry,
                    },
                ));
            }
        }
        features.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
        let collection = FeatureCollection {
            object_type: "FeatureCollection",
            frames_pathology: Metadata {
                schema_version: GEOJSON_SCHEMA_VERSION,
                coordinate_space: CoordinateSpace {
                    name: "SLIDE_BASE_PIXEL",
                    origin: "TOP_LEFT",
                    x_direction: "RIGHT",
                    y_direction: "DOWN",
                },
                source_identity: self.source_identity(),
                scheme: SchemeMetadata {
                    scheme_id: self.scheme().id(),
                    scheme_version: self.scheme().version(),
                    content_digest: self.scheme().content_digest(),
                },
            },
            features: features.into_iter().map(|(_, feature)| feature).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&collection).map_err(|error| {
            ViewerError::InvalidInput(format!(
                "scheme-aware pathology GeoJSON could not be encoded: {error}"
            ))
        })?;
        Ok(WorkspaceGeoJsonExport {
            bytes,
            excluded_measurement_count: self.measurements().len(),
        })
    }
}

fn polygon_geometry(mut components: Vec<PolygonComponent>) -> Result<Geometry> {
    if components.is_empty() {
        return Err(ViewerError::InvalidInput(
            "empty segment geometry cannot be exported to GeoJSON".into(),
        ));
    }
    let mut canonical = components
        .drain(..)
        .map(|component| {
            let exterior = canonical_ring(component.exterior(), true)?;
            let mut holes = component
                .holes()
                .iter()
                .map(|hole| canonical_ring(hole, false))
                .collect::<Result<Vec<_>>>()?;
            holes.sort_by(|left, right| compare_ring(left, right));
            Ok((exterior, holes))
        })
        .collect::<Result<Vec<_>>>()?;
    canonical.sort_by(|left, right| compare_ring(&left.0, &right.0));

    if canonical.len() == 1 {
        let (exterior, holes) = canonical.pop().expect("one canonical component exists");
        let mut rings = Vec::with_capacity(1 + holes.len());
        rings.push(exterior);
        rings.extend(holes);
        Ok(Geometry {
            geometry_type: "Polygon",
            coordinates: serde_json::to_value(rings).map_err(json_error)?,
        })
    } else {
        let polygons = canonical
            .into_iter()
            .map(|(exterior, holes)| {
                let mut rings = Vec::with_capacity(1 + holes.len());
                rings.push(exterior);
                rings.extend(holes);
                rings
            })
            .collect::<Vec<_>>();
        Ok(Geometry {
            geometry_type: "MultiPolygon",
            coordinates: serde_json::to_value(polygons).map_err(json_error)?,
        })
    }
}

fn canonical_ring(points: &[crate::Point2], exterior: bool) -> Result<Vec<[f64; 2]>> {
    if points.len() < 3 {
        return Err(ViewerError::InvalidInput(
            "GeoJSON polygon rings need at least three coordinates".into(),
        ));
    }
    let mut points = points.to_vec();
    let area = polygon_signed_area(&points);
    if !area.is_finite() || area.abs() < f64::EPSILON {
        return Err(ViewerError::InvalidInput(
            "GeoJSON polygon ring has zero or non-finite area".into(),
        ));
    }
    // In top-left slide coordinates, positive shoelace area is visually
    // clockwise. Exteriors use that winding and holes use the reverse.
    if (exterior && area < 0.0) || (!exterior && area > 0.0) {
        points.reverse();
    }
    let first = points
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| compare_point(left, right))
        .map(|(index, _)| index)
        .expect("a validated ring is nonempty");
    points.rotate_left(first);
    let mut coordinates = points
        .into_iter()
        .map(|point| [normalized_zero(point.x), normalized_zero(point.y)])
        .collect::<Vec<_>>();
    coordinates.push(coordinates[0]);
    Ok(coordinates)
}

fn compare_ring(left: &[[f64; 2]], right: &[[f64; 2]]) -> Ordering {
    left.iter()
        .zip(right)
        .find_map(|(left, right)| {
            let ordering = left[0]
                .total_cmp(&right[0])
                .then_with(|| left[1].total_cmp(&right[1]));
            (ordering != Ordering::Equal).then_some(ordering)
        })
        .unwrap_or_else(|| left.len().cmp(&right.len()))
}

fn compare_point(left: &crate::Point2, right: &crate::Point2) -> Ordering {
    left.x
        .total_cmp(&right.x)
        .then_with(|| left.y.total_cmp(&right.y))
}

fn normalized_zero(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value
    }
}

fn json_error(error: serde_json::Error) -> ViewerError {
    ViewerError::InvalidInput(format!("GeoJSON coordinates could not be encoded: {error}"))
}
