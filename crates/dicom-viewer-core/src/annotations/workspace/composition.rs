use geo::algorithm::bool_ops::FillRule;
use geo::{BooleanOps, Buffer, Coord, LineString, MultiPolygon, Point, Polygon};

use crate::{Point2, Result, ViewerError};

use super::model::{
    CompositeSegmentGeometry, PolygonComponent, SegmentOperation, SegmentationPrimitive,
    SegmentationPrimitiveGeometry,
};

pub(super) fn validate_primitive(primitive: &SegmentationPrimitive) -> Result<()> {
    match primitive.geometry() {
        SegmentationPrimitiveGeometry::Polygon { points } => {
            wsi_dicom_annotations::validate_polygon(points).map_err(Into::into)
        }
        SegmentationPrimitiveGeometry::Brush {
            centerline,
            diameter,
        } => {
            if centerline.is_empty() {
                return Err(ViewerError::InvalidInput(
                    "a brush primitive needs at least one centerline point".into(),
                ));
            }
            if !diameter.is_finite() || *diameter <= 0.0 {
                return Err(ViewerError::InvalidInput(
                    "brush diameter must be finite and positive".into(),
                ));
            }
            if centerline
                .iter()
                .any(|point| !point.x.is_finite() || !point.y.is_finite())
            {
                return Err(ViewerError::InvalidInput(
                    "brush centerline coordinates must be finite".into(),
                ));
            }
            Ok(())
        }
    }
}

pub(super) fn compose_segment(
    primitives: &[SegmentationPrimitive],
    dimensions: (u64, u64),
) -> Result<MultiPolygon<f64>> {
    let mut result = MultiPolygon::new(Vec::new());
    let clip = slide_bounds(dimensions)?;
    for primitive in primitives {
        validate_primitive(primitive)?;
        let geometry =
            primitive_geometry(primitive)?.intersection_with_fill_rule(&clip, FillRule::NonZero);
        result = match primitive.operation() {
            SegmentOperation::Add => result.union_with_fill_rule(&geometry, FillRule::NonZero),
            SegmentOperation::Erase => {
                result.difference_with_fill_rule(&geometry, FillRule::NonZero)
            }
        };
    }
    Ok(result.intersection_with_fill_rule(&clip, FillRule::NonZero))
}

pub(super) fn erase_intersects(
    current: &MultiPolygon<f64>,
    primitive: &SegmentationPrimitive,
    dimensions: (u64, u64),
) -> Result<bool> {
    let clip = slide_bounds(dimensions)?;
    let geometry =
        primitive_geometry(primitive)?.intersection_with_fill_rule(&clip, FillRule::NonZero);
    Ok(!current
        .intersection_with_fill_rule(&geometry, FillRule::NonZero)
        .0
        .is_empty())
}

pub(super) fn composite_geometry(geometry: MultiPolygon<f64>) -> CompositeSegmentGeometry {
    CompositeSegmentGeometry::new(
        geometry
            .0
            .into_iter()
            .filter_map(|polygon| {
                let exterior = ring_points(polygon.exterior());
                if exterior.len() < 3 {
                    return None;
                }
                let holes = polygon
                    .interiors()
                    .iter()
                    .map(ring_points)
                    .filter(|ring| ring.len() >= 3)
                    .collect();
                Some(PolygonComponent::new(exterior, holes))
            })
            .collect(),
    )
}

fn primitive_geometry(primitive: &SegmentationPrimitive) -> Result<MultiPolygon<f64>> {
    validate_primitive(primitive)?;
    Ok(match primitive.geometry() {
        SegmentationPrimitiveGeometry::Polygon { points } => {
            MultiPolygon::new(vec![polygon(points)])
        }
        SegmentationPrimitiveGeometry::Brush {
            centerline,
            diameter,
        } => {
            let radius = diameter * 0.5;
            if centerline.len() == 1 {
                Point::new(centerline[0].x, centerline[0].y).buffer(radius)
            } else {
                LineString::new(
                    centerline
                        .iter()
                        .map(|point| Coord {
                            x: point.x,
                            y: point.y,
                        })
                        .collect(),
                )
                .buffer(radius)
            }
        }
    })
}

fn slide_bounds(dimensions: (u64, u64)) -> Result<MultiPolygon<f64>> {
    if dimensions.0 == 0 || dimensions.1 == 0 {
        return Err(ViewerError::InvalidInput(
            "workspace source dimensions must be positive".into(),
        ));
    }
    let width = dimensions.0 as f64;
    let height = dimensions.1 as f64;
    Ok(MultiPolygon::new(vec![polygon(&[
        Point2::new(0.0, 0.0),
        Point2::new(width, 0.0),
        Point2::new(width, height),
        Point2::new(0.0, height),
    ])]))
}

fn polygon(points: &[Point2]) -> Polygon<f64> {
    let mut coordinates = points
        .iter()
        .map(|point| Coord {
            x: point.x,
            y: point.y,
        })
        .collect::<Vec<_>>();
    if coordinates.first() != coordinates.last() {
        coordinates.push(coordinates[0]);
    }
    Polygon::new(LineString::new(coordinates), Vec::new())
}

fn ring_points(ring: &LineString<f64>) -> Vec<Point2> {
    let mut points = ring
        .0
        .iter()
        .map(|coordinate| Point2::new(coordinate.x, coordinate.y))
        .collect::<Vec<_>>();
    if points.len() > 1 && points.first() == points.last() {
        points.pop();
    }
    points
}
