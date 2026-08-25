mod workspace;

pub use workspace::{
    CompositeSegmentGeometry, ControlledFindingSite, ExternalLayerKind, ExternalLayerReference,
    ExternalPromotionSource, LayerPresentation, PolygonComponent, SegmentEditOutcome,
    SegmentOperation, SegmentationLayer, SegmentationPrimitive, SegmentationPrimitiveGeometry,
    SegmentationSegmentFinding, SourceFrameContext, VectorFinding, VectorFindingGeometry,
    VectorLayer, VectorSegmentationPolicy, WorkspaceDocument, WorkspaceGeoJsonExport,
    WorkspaceLinearMeasurement, WorkspaceObjectProvenance, WorkspacePresentation,
};
pub use wsi_dicom_annotations::*;

/// Creates the explicit equipment identity used by Frames DICOM Viewer exports.
pub fn frames_viewer_producer(
    series_number: i32,
    series_description: &str,
) -> crate::Result<DerivedObjectProducer> {
    Ok(DerivedObjectProducer::new(
        series_number,
        "Frames",
        "DICOM Viewer",
        "not-applicable",
        env!("CARGO_PKG_VERSION"),
    )?
    .with_series_description(series_description)?)
}
