use crate::{AnnotationGroup, DicomAnnotationContext, DicomCode, Result, ViewerError};

use super::super::document::WorkspaceDocument;
use super::super::model::{ControlledFindingSite, SourceFrameContext, VectorFinding};

pub(in crate::annotations::workspace) fn validate_ann_source_context(
    object: &str,
    source_frame: &SourceFrameContext,
    context: &DicomAnnotationContext,
) -> Result<()> {
    let (z, c, t) = source_frame.plane();
    let axes = [("z", z), ("c", c), ("t", t)]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| format!("{name}={value}")))
        .collect::<Vec<_>>();
    if !axes.is_empty() {
        return Err(ViewerError::Unsupported(format!(
            "2D ANN cannot preserve source axes for {object}: {}",
            axes.join(", ")
        )));
    }
    if let Some(identifier) = source_frame.optical_path() {
        if !context
            .optical_path_identifiers()
            .iter()
            .any(|known| known == identifier)
        {
            return Err(ViewerError::InvalidInput(format!(
                "{object} references optical path {identifier:?}, which is not declared by the source WSI"
            )));
        }
    }
    Ok(())
}

pub(in crate::annotations::workspace) fn apply_vector_context(
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

pub(in crate::annotations::workspace) fn finding_site_code(
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

pub(in crate::annotations::workspace) fn dicom_label(label: &str) -> Result<String> {
    if label.trim().is_empty() || label.len() > 64 || label.contains(['\\', '\0']) {
        return Err(ViewerError::InvalidInput(
            "export label must be 1..=64 bytes and contain no DICOM separator or NUL".into(),
        ));
    }
    Ok(label.to_owned())
}

pub(in crate::annotations::workspace) fn validate_seg_source_context(
    object: &str,
    source_frame: &SourceFrameContext,
) -> Result<()> {
    let (z, c, t) = source_frame.plane();
    let mut values = Vec::new();
    if let Some(optical_path) = source_frame.optical_path() {
        values.push(format!("optical_path={optical_path:?}"));
    }
    for (name, value) in [("z", z), ("c", c), ("t", t)] {
        if let Some(value) = value {
            values.push(format!("{name}={value}"));
        }
    }
    if values.is_empty() {
        Ok(())
    } else {
        Err(ViewerError::Unsupported(format!(
            "SEG cannot preserve source context for {object}: {}",
            values.join(", ")
        )))
    }
}
