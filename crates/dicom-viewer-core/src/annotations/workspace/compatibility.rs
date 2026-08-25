use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    frames_viewer_producer, polygon_signed_area, AnnotationDocument, AnnotationGroup,
    AnnotationScheme, DicomAnnotationContext, DicomCode, Point2, Result, ViewerError,
};

use super::document::WorkspaceDocument;
use super::export::{apply_vector_context, dicom_label, finding_site_code};
use super::model::{PolygonComponent, VectorFindingGeometry};

const VIABLE_TUMOR_CIELAB: [u16; 3] = [49_152, 20_000, 48_000];
const EXCLUSION_CIELAB: [u16; 3] = [49_152, 48_000, 20_000];

/// Named adapter for the legacy CellViT++ viable-tumor GeoJSON contract.
///
/// This deliberately does not infer compatible private conventions from an
/// arbitrary scheme. The adapter is available only while the workspace is
/// pinned to the immutable `Tumor Mask Compatibility` v1 snapshot.
impl WorkspaceDocument {
    /// Exports the legacy private viable-tumor/exclusion ANN convention.
    ///
    /// This adapter is intentionally available only for the immutable Tumor
    /// Mask Compatibility v1 scheme. General ANN export never converts
    /// segmentation holes into private exclusion groups.
    pub fn export_tumor_mask_compatibility_ann(
        &self,
        context: &DicomAnnotationContext,
    ) -> Result<AnnotationDocument> {
        ensure_compatibility_scheme(self)?;
        let abnormal = DicomCode::new("49755003", "SCT", "Morphologically Abnormal Structure")?;
        let viable = DicomCode::new("VIABLE_TUMOR", "99FRAMES", "Viable tumor")?;
        let exclusion = DicomCode::new("EXCLUSION", "99FRAMES", "Excluded region")?;
        let mut groups = Vec::new();

        for finding in self.vector_findings() {
            let class = self.scheme().class(finding.class_id()).ok_or_else(|| {
                ViewerError::InvalidInput(
                    "finding references an unknown compatibility class".into(),
                )
            })?;
            let group = match (finding.class_id(), finding.geometry()) {
                ("viable-tumor", VectorFindingGeometry::Regions(components)) => {
                    AnnotationGroup::polygons(
                        dicom_label(finding.name().unwrap_or("Viable tumor outlines"))?,
                        abnormal.clone(),
                        viable.clone(),
                        VIABLE_TUMOR_CIELAB,
                        components
                            .iter()
                            .map(|component| component.to_vec())
                            .collect(),
                    )?
                }
                ("cell", VectorFindingGeometry::Point(point)) => AnnotationGroup::points(
                    dicom_label(finding.name().unwrap_or(class.label()))?,
                    class.category().clone(),
                    class.property_type().clone(),
                    class.recommended_display_cielab(),
                    vec![*point],
                )?,
                _ => {
                    return Err(ViewerError::InvalidInput(
                        "compatibility finding geometry does not match its controlled class".into(),
                    ));
                }
            }
            .with_uid(finding.tracking().uid())?;
            groups.push(apply_vector_context(self, finding, group)?);
        }

        for segment in self.segments() {
            if segment.class_id() != "viable-tumor" {
                return Err(ViewerError::InvalidInput(
                    "compatibility segmentation contains a non-viable-tumor segment".into(),
                ));
            }
            let geometry = self.composite_segment(segment.object_id())?;
            let mut viable_group = AnnotationGroup::polygons(
                dicom_label(segment.name().unwrap_or("Viable tumor outlines"))?,
                abnormal.clone(),
                viable.clone(),
                VIABLE_TUMOR_CIELAB,
                geometry
                    .components()
                    .iter()
                    .map(|component| component.exterior().to_vec())
                    .collect(),
            )?
            .with_uid(segment.tracking().uid())?;
            viable_group = apply_segment_context(self, segment, viable_group)?;
            groups.push(viable_group);

            let holes = geometry
                .components()
                .iter()
                .flat_map(|component| component.holes().iter().cloned())
                .collect::<Vec<_>>();
            if !holes.is_empty() {
                let mut exclusion_group = AnnotationGroup::polygons(
                    "Exclusion outlines",
                    abnormal.clone(),
                    exclusion.clone(),
                    EXCLUSION_CIELAB,
                    holes,
                )?
                .with_uid(derived_exclusion_uid(segment.object_id()))?;
                exclusion_group = apply_segment_context(self, segment, exclusion_group)?;
                groups.push(exclusion_group);
            }
        }

        if groups.is_empty() {
            return Err(ViewerError::InvalidInput(
                "compatibility ANN export has no viable-tumor or cell content".into(),
            ));
        }
        Ok(AnnotationDocument::new(context.clone(), groups)?
            .with_producer(frames_viewer_producer(9101, "WSI annotations")?))
    }

    pub fn export_cellvit_compatibility_geojson(&self) -> Result<Vec<u8>> {
        ensure_compatibility_scheme(self)?;

        let mut components = Vec::<(u64, usize, PolygonComponent)>::new();
        for finding in self.vector_findings() {
            if finding.class_id() != "viable-tumor" {
                continue;
            }
            let VectorFindingGeometry::Regions(regions) = finding.geometry() else {
                continue;
            };
            components.extend(regions.iter().enumerate().map(|(index, exterior)| {
                (
                    finding.ordinal(),
                    index,
                    PolygonComponent::new(exterior.to_vec(), Vec::new()),
                )
            }));
        }
        for segment in self.segments() {
            if segment.class_id() != "viable-tumor" {
                continue;
            }
            components.extend(
                self.composite_segment(segment.object_id())?
                    .components()
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, component)| (segment.ordinal(), index, component)),
            );
        }
        components.sort_by_key(|(ordinal, index, _)| (*ordinal, *index));
        if components.is_empty() {
            return Err(ViewerError::InvalidInput(
                "CellViT compatibility export has no viable-tumor regions".into(),
            ));
        }

        let features = components
            .into_iter()
            .enumerate()
            .map(|(index, (_, _, component))| {
                let fragment_id = format!("F{:03}", index + 1);
                let mut rings = vec![closed_ring(component.exterior(), true)];
                rings.extend(
                    component
                        .holes()
                        .iter()
                        .map(|hole| closed_ring(hole, false)),
                );
                json!({
                    "type": "Feature",
                    "properties": {
                        "objectType": "annotation",
                        "name": fragment_id,
                        "fragment_id": fragment_id,
                        "coordinate_space": "level-0_pixels",
                        "classification": { "name": "viable_tumor" }
                    },
                    "geometry": {
                        "type": "Polygon",
                        "coordinates": rings
                    }
                })
            })
            .collect::<Vec<Value>>();
        serde_json::to_vec_pretty(&json!({
            "type": "FeatureCollection",
            "features": features
        }))
        .map_err(|error| {
            ViewerError::InvalidInput(format!(
                "CellViT compatibility GeoJSON could not be encoded: {error}"
            ))
        })
    }
}

fn ensure_compatibility_scheme(document: &WorkspaceDocument) -> Result<()> {
    let compatibility = AnnotationScheme::tumor_mask_compatibility_v1();
    if document.scheme().content_digest() != compatibility.content_digest() {
        return Err(ViewerError::InvalidInput(
            "compatibility export requires Tumor Mask Compatibility v1".into(),
        ));
    }
    Ok(())
}

fn apply_segment_context(
    document: &WorkspaceDocument,
    segment: &super::model::SegmentationSegmentFinding,
    mut group: AnnotationGroup,
) -> Result<AnnotationGroup> {
    if let Some(comment) = segment.comment() {
        group = group.with_description(comment)?;
    }
    if let Some(site) = segment.finding_site() {
        group = group.with_anatomic_regions(vec![finding_site_code(document, site)?]);
    }
    if let Some(optical_path) = segment.source_frame().optical_path() {
        group = group.with_referenced_optical_paths(vec![optical_path.to_owned()])?;
    }
    Ok(group)
}

fn derived_exclusion_uid(object_id: uuid::Uuid) -> String {
    let digest = Sha256::digest(
        [
            b"frames-tumor-mask-exclusion-v1\0".as_slice(),
            object_id.as_bytes(),
        ]
        .concat(),
    );
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    format!("2.25.{}", u128::from_be_bytes(bytes))
}

fn closed_ring(points: &[Point2], positive_area: bool) -> Vec<[f64; 2]> {
    let mut ordered = points.to_vec();
    if (polygon_signed_area(&ordered) > 0.0) != positive_area {
        ordered.reverse();
    }
    ordered.push(ordered[0]);
    ordered
        .into_iter()
        .map(|point| [point.x, point.y])
        .collect()
}
