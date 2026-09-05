use crate::{
    frames_viewer_producer, AnnotationDocument, AnnotationGroup, DicomAnnotationContext, Result,
    ViewerError,
};

use super::super::document::WorkspaceDocument;
use super::super::model::VectorFindingGeometry;
use super::shared::{apply_vector_context, dicom_label, validate_ann_source_context};

impl WorkspaceDocument {
    pub fn export_ann(&self, context: &DicomAnnotationContext) -> Result<AnnotationDocument> {
        let mut groups = Vec::new();
        for finding in self.vector_findings() {
            validate_ann_source_context(
                &format!("vector finding #{}", finding.ordinal()),
                finding.source_frame(),
                context,
            )?;
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
}
