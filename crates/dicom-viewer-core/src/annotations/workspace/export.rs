use crate::{
    frames_viewer_producer, AnnotationDocument, AnnotationGroup, DicomAnnotationContext, DicomCode,
    Result, SegmentationDocument, SegmentationSegment, ViewerError,
};

use super::document::WorkspaceDocument;
use super::model::{ControlledFindingSite, VectorFinding, VectorFindingGeometry};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VectorSegmentationPolicy {
    #[default]
    Exclude,
    Rasterize,
}

impl WorkspaceDocument {
    pub fn export_ann(&self, context: &DicomAnnotationContext) -> Result<AnnotationDocument> {
        let mut groups = Vec::new();
        for finding in self.vector_findings() {
            let class = self.scheme().class(finding.class_id()).ok_or_else(|| {
                ViewerError::InvalidInput("finding references an unknown annotation class".into())
            })?;
            let label = dicom_label(finding.name().unwrap_or_else(|| class.label()))?;
            let group = match finding.geometry() {
                VectorFindingGeometry::Point(point) => AnnotationGroup::points(
                    label,
                    class.category().clone(),
                    class.property_type().clone(),
                    class.recommended_display_cielab(),
                    vec![*point],
                )?,
                VectorFindingGeometry::Regions(components) => AnnotationGroup::polygons(
                    label,
                    class.category().clone(),
                    class.property_type().clone(),
                    class.recommended_display_cielab(),
                    components
                        .iter()
                        .map(|component| component.to_vec())
                        .collect(),
                )?,
            }
            .with_uid(finding.tracking().uid())?
            .with_property_type_modifiers(class.property_type_modifiers().to_vec());
            groups.push(apply_vector_context(self, finding, group)?);
        }
        if groups.is_empty() {
            return Err(ViewerError::InvalidInput(
                "ANN export has no directly representable vector findings".into(),
            ));
        }
        Ok(AnnotationDocument::new(context.clone(), groups)?
            .with_producer(frames_viewer_producer(9101, "WSI annotations")?))
    }

    pub fn export_seg(
        &self,
        context: &DicomAnnotationContext,
        vector_policy: VectorSegmentationPolicy,
    ) -> Result<SegmentationDocument> {
        let mut segments = Vec::new();
        for segment in self.segments() {
            let class = self.scheme().class(segment.class_id()).ok_or_else(|| {
                ViewerError::InvalidInput("segment references an unknown annotation class".into())
            })?;
            let geometry = self.composite_segment(segment.object_id())?;
            if geometry.components().is_empty() {
                return Err(ViewerError::InvalidInput(format!(
                    "segment #{} is empty and cannot be exported",
                    segment.ordinal()
                )));
            }
            let outer = geometry
                .components()
                .iter()
                .map(|component| component.exterior().to_vec())
                .collect::<Vec<_>>();
            let holes = geometry
                .components()
                .iter()
                .map(|component| component.holes().to_vec())
                .collect::<Vec<_>>();
            let mut exported = SegmentationSegment::new(
                dicom_label(segment.name().unwrap_or_else(|| class.label()))?,
                class.category().clone(),
                class.property_type().clone(),
                class.recommended_display_cielab(),
                outer,
                Vec::new(),
            )?
            .with_component_holes(holes)?
            .with_property_type_modifiers(class.property_type_modifiers().to_vec())
            .with_tracking(segment.tracking().id(), segment.tracking().uid())?;
            if let Some(comment) = segment.comment() {
                exported = exported.with_description(comment)?;
            }
            if let Some(site) = segment.finding_site() {
                exported = exported.with_anatomic_regions(vec![finding_site_code(self, site)?]);
            }
            segments.push(exported);
        }

        if vector_policy == VectorSegmentationPolicy::Rasterize {
            for finding in self.vector_findings() {
                let VectorFindingGeometry::Regions(components) = finding.geometry() else {
                    continue;
                };
                let class = self.scheme().class(finding.class_id()).ok_or_else(|| {
                    ViewerError::InvalidInput(
                        "finding references an unknown annotation class".into(),
                    )
                })?;
                let mut exported = SegmentationSegment::new(
                    dicom_label(finding.name().unwrap_or_else(|| class.label()))?,
                    class.category().clone(),
                    class.property_type().clone(),
                    class.recommended_display_cielab(),
                    components
                        .iter()
                        .map(|component| component.to_vec())
                        .collect(),
                    Vec::new(),
                )?
                .with_property_type_modifiers(class.property_type_modifiers().to_vec())
                .with_tracking(finding.tracking().id(), finding.tracking().uid())?;
                if let Some(comment) = finding.comment() {
                    exported = exported.with_description(comment)?;
                }
                if let Some(site) = finding.finding_site() {
                    exported = exported.with_anatomic_regions(vec![finding_site_code(self, site)?]);
                }
                segments.push(exported);
            }
        }

        if segments.is_empty() {
            return Err(ViewerError::InvalidInput(
                "SEG export has no editable segmentation content".into(),
            ));
        }
        Ok(SegmentationDocument::binary(context.clone(), segments)?
            .with_producer(frames_viewer_producer(9201, "WSI segmentations")?))
    }
}

pub(super) fn apply_vector_context(
    document: &WorkspaceDocument,
    finding: &VectorFinding,
    mut group: AnnotationGroup,
) -> Result<AnnotationGroup> {
    if let Some(comment) = finding.comment() {
        group = group.with_description(comment)?;
    }
    if let Some(site) = finding.finding_site() {
        group = group.with_anatomic_regions(vec![finding_site_code(document, site)?]);
    }
    if let Some(optical_path) = finding.source_frame().optical_path() {
        group = group.with_referenced_optical_paths(vec![optical_path.to_owned()])?;
    }
    Ok(group)
}

pub(super) fn finding_site_code(
    document: &WorkspaceDocument,
    site: &ControlledFindingSite,
) -> Result<DicomCode> {
    document
        .scheme()
        .finding_sites()
        .iter()
        .find(|code| site.matches(code))
        .cloned()
        .ok_or_else(|| {
            ViewerError::InvalidInput(
                "finding site is not controlled by the pinned annotation scheme".into(),
            )
        })
}

pub(super) fn dicom_label(label: &str) -> Result<String> {
    if label.trim().is_empty() || label.len() > 64 || label.contains(['\\', '\0']) {
        return Err(ViewerError::InvalidInput(
            "export label must be 1..=64 bytes and contain no DICOM separator or NUL".into(),
        ));
    }
    Ok(label.to_owned())
}
