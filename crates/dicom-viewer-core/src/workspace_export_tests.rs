use crate::annotation_test_support::write_source_wsi;
use crate::{
    AnnotationScheme, DicomAnnotationContext, Point2, SegmentOperation, SegmentationPrimitive,
    VectorFindingGeometry, VectorSegmentationPolicy, ViewerSourceIdentity, WorkspaceDocument,
};

fn square(x: f64, y: f64, size: f64) -> Vec<Point2> {
    vec![
        Point2::new(x, y),
        Point2::new(x + size, y),
        Point2::new(x + size, y + size),
        Point2::new(x, y + size),
    ]
}

fn workspace() -> WorkspaceDocument {
    WorkspaceDocument::new(
        ViewerSourceIdentity::new(71, 0, 0, 0, 0, 0, (256, 256)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap()
}

#[test]
fn scheme_aware_geojson_has_one_canonical_feature_per_tracked_object() {
    let mut document = workspace();
    let vector_layer = document.vector_layers()[0].id();
    let first = document
        .add_vector_finding(
            vector_layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(10.0, 10.0, 20.0)]),
        )
        .unwrap();
    let second = document
        .add_vector_finding(
            vector_layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(100.0, 10.0, 20.0)]),
        )
        .unwrap();
    let segment_layer = document.ensure_manual_segmentation_layer();
    let segment = document
        .add_segment(
            segment_layer,
            "necrosis",
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(30.0, 50.0, 80.0)),
        )
        .unwrap();
    document
        .apply_segment_primitive(
            segment,
            SegmentationPrimitive::polygon(SegmentOperation::Erase, square(50.0, 70.0, 20.0)),
        )
        .unwrap();
    document
        .add_linear_measurement(
            "neoplasm",
            [Point2::new(0.0, 0.0), Point2::new(3.0, 4.0)],
            Some(0.00125),
        )
        .unwrap();

    let first_export = document.export_pathology_geojson().unwrap();
    let second_export = document.export_pathology_geojson().unwrap();
    assert_eq!(first_export.bytes(), second_export.bytes());
    assert_eq!(first_export.excluded_measurement_count(), 1);
    let value: serde_json::Value = serde_json::from_slice(first_export.bytes()).unwrap();
    assert_eq!(
        value["frames_pathology"]["schema_version"],
        "frames-pathology-geojson-v1"
    );
    assert_eq!(
        value["frames_pathology"]["coordinate_space"]["name"],
        "SLIDE_BASE_PIXEL"
    );
    assert_eq!(
        value["frames_pathology"]["coordinate_space"]["origin"],
        "TOP_LEFT"
    );
    assert_eq!(
        value["frames_pathology"]["scheme"]["content_digest"],
        document.scheme().content_digest()
    );
    let features = value["features"].as_array().unwrap();
    assert_eq!(features.len(), 3);
    let ids = features
        .iter()
        .map(|feature| feature["properties"]["object_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.contains(&first.to_string().as_str()));
    assert!(ids.contains(&second.to_string().as_str()));
    let segment_feature = features
        .iter()
        .find(|feature| feature["properties"]["object_id"] == segment.to_string())
        .unwrap();
    assert_eq!(segment_feature["geometry"]["type"], "Polygon");
    assert_eq!(
        segment_feature["geometry"]["coordinates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn ann_exports_independent_vector_findings_only_and_reuses_tracking_uid() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi(&source, 256, 256, 64, 64);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let mut document = workspace();
    let layer = document.vector_layers()[0].id();
    let finding = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(10.0, 10.0, 20.0)]),
        )
        .unwrap();
    let segment_layer = document.ensure_manual_segmentation_layer();
    document
        .add_segment(
            segment_layer,
            "neoplasm",
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(80.0, 80.0, 20.0)),
        )
        .unwrap();

    let ann = document.export_ann(&context).unwrap();
    assert_eq!(ann.producer().manufacturer(), "Frames");
    assert_eq!(ann.producer().manufacturer_model_name(), "DICOM Viewer");
    assert_eq!(ann.producer().series_number(), "9101");
    assert_eq!(ann.groups().len(), 1);
    assert_eq!(ann.groups()[0].annotation_count(), 1);
    assert_eq!(
        ann.groups()[0].uid(),
        document.finding(finding).unwrap().tracking().uid()
    );
}

#[test]
fn seg_keeps_same_class_segments_separate_and_vector_rasterization_is_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi(&source, 256, 256, 64, 64);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let mut document = workspace();
    let vector_layer = document.vector_layers()[0].id();
    let vector = document
        .add_vector_finding(
            vector_layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(150.0, 150.0, 20.0)]),
        )
        .unwrap();
    let layer = document.ensure_manual_segmentation_layer();
    let first = document
        .add_segment(
            layer,
            "neoplasm",
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(10.0, 10.0, 20.0)),
        )
        .unwrap();
    let second = document
        .add_segment(
            layer,
            "neoplasm",
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(50.0, 10.0, 20.0)),
        )
        .unwrap();

    let direct = document
        .export_seg(&context, VectorSegmentationPolicy::Exclude)
        .unwrap();
    assert_eq!(direct.producer().manufacturer(), "Frames");
    assert_eq!(direct.producer().manufacturer_model_name(), "DICOM Viewer");
    assert_eq!(direct.producer().series_number(), "9201");
    assert_eq!(direct.segments().len(), 2);
    assert_eq!(
        direct.segments()[0].tracking_uid(),
        Some(document.segment(first).unwrap().tracking().uid())
    );
    assert_eq!(
        direct.segments()[1].tracking_uid(),
        Some(document.segment(second).unwrap().tracking().uid())
    );

    let rasterized = document
        .export_seg(&context, VectorSegmentationPolicy::Rasterize)
        .unwrap();
    assert_eq!(rasterized.segments().len(), 3);
    assert_eq!(
        rasterized.segments()[2].tracking_uid(),
        Some(document.finding(vector).unwrap().tracking().uid())
    );
    assert_ne!(direct.sop_instance_uid(), rasterized.sop_instance_uid());
}

#[test]
fn tumor_mask_compatibility_geojson_keeps_exact_cellvit_contract() {
    let mut document = WorkspaceDocument::new(
        ViewerSourceIdentity::new(72, 0, 0, 0, 0, 0, (256, 256)),
        AnnotationScheme::tumor_mask_compatibility_v1(),
    )
    .unwrap();
    let layer = document.ensure_manual_segmentation_layer();
    let segment = document
        .add_segment(
            layer,
            "viable-tumor",
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(10.0, 10.0, 100.0)),
        )
        .unwrap();
    document
        .apply_segment_primitive(
            segment,
            SegmentationPrimitive::polygon(SegmentOperation::Erase, square(30.0, 30.0, 20.0)),
        )
        .unwrap();

    let bytes = document.export_cellvit_compatibility_geojson().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["type"], "FeatureCollection");
    assert!(value.get("frames_pathology").is_none());
    let feature = &value["features"][0];
    assert_eq!(feature["properties"]["objectType"], "annotation");
    assert_eq!(feature["properties"]["name"], "F001");
    assert_eq!(feature["properties"]["fragment_id"], "F001");
    assert_eq!(feature["properties"]["coordinate_space"], "level-0_pixels");
    assert_eq!(
        feature["properties"]["classification"]["name"],
        "viable_tumor"
    );
    assert_eq!(feature["geometry"]["type"], "Polygon");
    let rings = feature["geometry"]["coordinates"].as_array().unwrap();
    assert_eq!(rings.len(), 2);
    for ring in rings {
        let points = ring.as_array().unwrap();
        assert_eq!(points.first(), points.last());
    }
}

#[test]
fn named_compatibility_ann_preserves_viable_tumor_and_exclusion_codes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi(&source, 256, 256, 64, 64);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let mut document = WorkspaceDocument::new(
        ViewerSourceIdentity::new(73, 0, 0, 0, 0, 0, (256, 256)),
        AnnotationScheme::tumor_mask_compatibility_v1(),
    )
    .unwrap();
    let layer = document.ensure_manual_segmentation_layer();
    let segment = document
        .add_segment(
            layer,
            "viable-tumor",
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(10.0, 10.0, 100.0)),
        )
        .unwrap();
    document
        .apply_segment_primitive(
            segment,
            SegmentationPrimitive::polygon(SegmentOperation::Erase, square(30.0, 30.0, 20.0)),
        )
        .unwrap();

    let ann = document
        .export_tumor_mask_compatibility_ann(&context)
        .unwrap();
    assert_eq!(ann.producer().manufacturer(), "Frames");
    assert_eq!(ann.producer().manufacturer_model_name(), "DICOM Viewer");
    assert_eq!(ann.groups().len(), 2);
    assert_eq!(ann.groups()[0].property_type().value(), "VIABLE_TUMOR");
    assert_eq!(ann.groups()[1].property_type().value(), "EXCLUSION");
    assert_eq!(ann.groups()[0].annotation_count(), 1);
    assert_eq!(ann.groups()[1].annotation_count(), 1);
    assert_eq!(
        ann.groups()[0].uid(),
        document.segment(segment).unwrap().tracking().uid()
    );
}
