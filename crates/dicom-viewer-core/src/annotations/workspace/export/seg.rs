use crate::{
    frames_viewer_producer, DicomAnnotationContext, Result, SegmentationDocument,
    SegmentationSegment, ViewerError,
};

use super::super::document::WorkspaceDocument;
use super::super::model::VectorFindingGeometry;
use super::shared::{dicom_label, finding_site_code, validate_seg_source_context};
use super::VectorSegmentationPolicy;

impl WorkspaceDocument {
    pub fn export_seg(
        &self,
        context: &DicomAnnotationContext,
        vector_policy: VectorSegmentationPolicy,
    ) -> Result<SegmentationDocument> {
        let mut segments = Vec::new();
        for segment in self.segments() {
            validate_seg_source_context(
                &format!("segment #{}", segment.ordinal()),
                segment.source_frame(),
            )?;
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
                validate_seg_source_context(
                    &format!("vector finding #{}", finding.ordinal()),
                    finding.source_frame(),
                )?;
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
