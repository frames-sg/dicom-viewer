//! Immutable lookup data owned by a loaded external payload. Coordinates already
//! in level-zero space remain in their source payload; only read-only DICOM
//! coordinates need a prepared canonical copy.
use dicom_viewer_core::{
    AnnotationGeometry, AnnotationGraphicType, AnnotationGroup, DicomAnnotationContext, Point2,
};
use rstar::{RTree, RTreeObject, AABB};

use super::ExternalLayerPayload;

#[derive(Debug)]
struct IndexedPrimitive {
    index: usize,
    envelope: AABB<[f64; 2]>,
}
impl RTreeObject for IndexedPrimitive {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        self.envelope
    }
}

#[derive(Debug, Default)]
pub(super) struct BoundsIndex(RTree<IndexedPrimitive>);
impl BoundsIndex {
    pub(super) fn new(bounds: impl IntoIterator<Item = Option<[f64; 4]>>) -> Self {
        Self(RTree::bulk_load(
            bounds
                .into_iter()
                .enumerate()
                .filter_map(|(index, bounds)| {
                    let b = bounds?;
                    b.iter().all(|v| v.is_finite()).then_some(IndexedPrimitive {
                        index,
                        envelope: AABB::from_corners([b[0], b[1]], [b[2], b[3]]),
                    })
                })
                .collect(),
        ))
    }
    pub(super) fn visible(&self, bounds: [f64; 4]) -> Vec<usize> {
        let mut indices: Vec<_> = self
            .0
            .locate_in_envelope_intersecting(&AABB::from_corners(
                [bounds[0], bounds[1]],
                [bounds[2], bounds[3]],
            ))
            .map(|entry| entry.index)
            .collect();
        // Preserve source draw order, including overlapping colors and alpha.
        indices.sort_unstable();
        indices
    }
}

#[derive(Debug, Default)]
pub(super) struct PreparedGroup {
    pub(super) index: BoundsIndex,
    pub(super) canonical: Vec<Point2>,
    pub(super) ranges: Vec<(usize, usize)>,
}

impl PreparedGroup {
    pub(super) fn new(
        group: &AnnotationGroup,
        imported: Option<(
            &dicom_viewer_core::AnnotationDocument,
            &DicomAnnotationContext,
        )>,
    ) -> Self {
        match group.geometry() {
            AnnotationGeometry::Points(points) => Self {
                index: BoundsIndex::new(points.iter().map(|p| Some([p.x, p.y, p.x, p.y]))),
                ..Self::default()
            },
            AnnotationGeometry::Polygons(polygons) => Self {
                index: BoundsIndex::new(polygons.iter().map(|p| point_bounds(p))),
                ..Self::default()
            },
            AnnotationGeometry::ReadOnly {
                graphic_type,
                coordinates,
                primitive_point_indices,
                coordinate_dimensions,
            } => {
                let Some((document, source)) = imported else {
                    return Self::default();
                };
                let canonical = coordinates
                    .chunks_exact(*coordinate_dimensions)
                    .map(|p| document.canonical_level0_pixel(source, p[0], p[1], p.get(2).copied()))
                    .collect::<Result<Vec<_>, _>>();
                // The previous renderer omitted groups whose coordinate mapping
                // fails; keep that behavior and cache the omitted result.
                let Ok(canonical) = canonical else {
                    return Self::default();
                };
                let ranges: Vec<_> = if *graphic_type == AnnotationGraphicType::Point {
                    (0..canonical.len()).map(|i| (i, i + 1)).collect()
                } else {
                    super::external_overlay::primitive_ranges(
                        primitive_point_indices,
                        canonical.len(),
                    )
                };
                let index = BoundsIndex::new(
                    ranges
                        .iter()
                        .map(|&(start, end)| point_bounds(&canonical[start..end])),
                );
                Self {
                    index,
                    canonical,
                    ranges,
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct PreparedExternalLayer {
    pub(super) groups: Vec<PreparedGroup>,
    pub(super) features: BoundsIndex,
    pub(super) mask_runs: BoundsIndex,
    pub(super) regions: BoundsIndex,
    pub(super) frames: BoundsIndex,
    pub(super) frame_maxima: Vec<u16>,
}
impl PreparedExternalLayer {
    pub(super) fn new(payload: &ExternalLayerPayload, source: &DicomAnnotationContext) -> Self {
        let mut prepared = Self::default();
        match payload {
            ExternalLayerPayload::Annotation(document) => {
                prepared.groups = document
                    .groups()
                    .iter()
                    .map(|group| PreparedGroup::new(group, Some((document, source))))
                    .collect()
            }
            ExternalLayerPayload::Segmentation {
                document,
                vector_groups,
            } => {
                if let Some(groups) = vector_groups {
                    prepared.groups = groups
                        .iter()
                        .map(|group| PreparedGroup::new(group, None))
                        .collect();
                }
                if let Some(frames) = document.fractional_frames() {
                    prepared.frames = BoundsIndex::new(frames.iter().map(|frame| {
                        let (w, h) = frame.dimensions();
                        let x = frame.tile_col() * u32::from(w);
                        let y = frame.tile_row() * u32::from(h);
                        Some([
                            f64::from(x),
                            f64::from(y),
                            f64::from(x.saturating_add(u32::from(w))),
                            f64::from(y.saturating_add(u32::from(h))),
                        ])
                    }));
                    prepared.frame_maxima = frames
                        .iter()
                        .map(|frame| frame.values().iter().copied().max().unwrap_or(0))
                        .collect();
                }
            }
            ExternalLayerPayload::ProfiledGeoJson(session) => {
                prepared.features = BoundsIndex::new(
                    session
                        .preview()
                        .features()
                        .iter()
                        .map(|feature| Some(feature.bounds())),
                )
            }
            ExternalLayerPayload::Report(session) => {
                prepared.regions =
                    BoundsIndex::new(session.regions().iter().map(|region| Some(region.bounds())));
                prepared.mask_runs = BoundsIndex::new(session.mask_runs().iter().map(|run| {
                    let length = match &run.values {
                        crate::app::report::ReportMaskValues::Binary(length) => *length,
                        crate::app::report::ReportMaskValues::Fractional { values, .. } => {
                            u32::try_from(values.len()).unwrap_or(u32::MAX)
                        }
                    };
                    Some([
                        f64::from(run.column_start),
                        f64::from(run.row),
                        f64::from(run.column_start.saturating_add(length)),
                        f64::from(run.row.saturating_add(1)),
                    ])
                }));
            }
            ExternalLayerPayload::Heatmap { .. } => {}
        }
        prepared
    }
}

fn point_bounds(points: &[Point2]) -> Option<[f64; 4]> {
    let first = points.first()?;
    Some(
        points
            .iter()
            .skip(1)
            .fold([first.x, first.y, first.x, first.y], |b, p| {
                [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)]
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viewport_keeps_crossing_primitives_in_source_order() {
        let index = BoundsIndex::new([
            Some([1000.0, 0.0, 1001.0, 1.0]),
            Some([-100.0, -100.0, 100.0, 100.0]),
            None,
            Some([-100.0, 1.0, 100.0, 1.0]),
            Some([1.0, 1.0, 1.0, 1.0]),
        ]);
        assert_eq!(index.visible([0.0, 0.0, 5.0, 5.0]), [1, 3, 4]);
    }
}
