use crate::annotation_test_support::{write_source_wsi, write_source_wsi_with_optical_paths};
use crate::{
    AlgorithmIdentification, AnnotationDocument, AnnotationScheme, DicomAnnotationContext,
    DicomCode, ExternalLayerKind, ExternalLayerReference, ExternalPromotionSource, GenerationType,
    Point2, SegmentOperation, SegmentationPrimitive, SourceFrameContext, VectorFindingGeometry,
    VectorSegmentationPolicy, ViewerSourceIdentity, WorkspaceDocument,
};

fn automatic_algorithm() -> AlgorithmIdentification {
    AlgorithmIdentification::new(
        DicomCode::new("AI", "99FRAMES", "Artificial intelligence").unwrap(),
        "bulk exporter",
        "1.0",
    )
    .unwrap()
}

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
fn automatic_bulk_ann_groups_polygons_and_returns_exact_locations() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    let ann_path = temp.path().join("bulk.dcm");
    write_source_wsi(&source, 256, 256, 64, 64);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let mut document = workspace();
    let layer = document.vector_layers()[0].id();
    let first = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![
                square(10.0, 10.0, 10.0),
                square(30.0, 10.0, 10.0),
            ]),
        )
        .unwrap();
    let second = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(50.0, 10.0, 10.0)]),
        )
        .unwrap();

    let first_export = document
        .export_automatic_bulk_ann(&context, automatic_algorithm())
        .unwrap();
    let second_export = document
        .export_automatic_bulk_ann(&context, automatic_algorithm())
        .unwrap();

    assert_eq!(first_export.document().groups().len(), 1);
    let group = &first_export.document().groups()[0];
    assert_eq!(group.annotation_count(), 3);
    assert_eq!(group.generation_type(), GenerationType::Automatic);
    assert_eq!(group.algorithms(), &[automatic_algorithm()]);
    assert_eq!(group.uid(), second_export.document().groups()[0].uid());
    let locations = first_export.annotation_locations();
    assert_eq!(locations.len(), 2);
    assert_eq!(locations[0].object_id(), first);
    assert_eq!(
        locations[0].tracking_uid(),
        document.finding(first).unwrap().tracking().uid()
    );
    assert_eq!(locations[0].group_uid(), group.uid());
    assert_eq!(locations[0].first_annotation_index().get(), 1);
    assert_eq!(locations[0].annotation_count().get(), 2);
    assert_eq!(locations[1].object_id(), second);
    assert_eq!(locations[1].group_uid(), group.uid());
    assert_eq!(locations[1].first_annotation_index().get(), 3);
    assert_eq!(locations[1].annotation_count().get(), 1);
    first_export.document().write_ann(&ann_path).unwrap();
    let restored = AnnotationDocument::read_ann(&ann_path, &context).unwrap();
    for location in locations {
        let restored_group = restored
            .groups()
            .iter()
            .find(|candidate| candidate.uid() == location.group_uid())
            .unwrap();
        let start = usize::try_from(location.first_annotation_index().get() - 1).unwrap();
        let end = start + usize::try_from(location.annotation_count().get()).unwrap();
        let expected = match document.finding(location.object_id()).unwrap().geometry() {
            VectorFindingGeometry::Regions(components) => components
                .iter()
                .map(|component| component.to_vec())
                .collect::<Vec<_>>(),
            VectorFindingGeometry::Point(_) => unreachable!(),
        };
        assert_eq!(
            &restored_group.polygon_annotations().unwrap()[start..end],
            expected
        );
    }

    let singleton = document.export_ann(&context).unwrap();
    assert_eq!(singleton.groups().len(), 2);
    assert_eq!(
        singleton.groups()[0].uid(),
        document.finding(first).unwrap().tracking().uid()
    );
    assert_eq!(
        singleton.groups()[1].uid(),
        document.finding(second).unwrap().tracking().uid()
    );
}

#[test]
fn automatic_bulk_ann_separates_classes_and_graphic_types() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi(&source, 256, 256, 64, 64);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let mut document = workspace();
    let layer = document.vector_layers()[0].id();
    document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(10.0, 10.0, 10.0)]),
        )
        .unwrap();
    document
        .add_vector_finding(
            layer,
            "necrosis",
            VectorFindingGeometry::regions(vec![square(30.0, 10.0, 10.0)]),
        )
        .unwrap();
    document
        .add_vector_finding(
            layer,
            "cell",
            VectorFindingGeometry::Point(Point2::new(50.0, 10.0)),
        )
        .unwrap();

    let export = document
        .export_automatic_bulk_ann(&context, automatic_algorithm())
        .unwrap();

    assert_eq!(export.document().groups().len(), 3);
    assert_eq!(export.document().groups()[0].label(), "Neoplasm");
    assert!(export.document().groups()[0]
        .polygon_annotations()
        .is_some());
    assert_eq!(export.document().groups()[1].label(), "Necrosis");
    assert!(export.document().groups()[1]
        .polygon_annotations()
        .is_some());
    assert_eq!(export.document().groups()[2].label(), "Cell");
    assert!(export.document().groups()[2].point_annotations().is_some());
}

#[test]
fn automatic_bulk_ann_separates_site_and_known_optical_path_context() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi_with_optical_paths(&source, 256, 256, 64, 64, &["A", "B"]);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let mut scheme_json: serde_json::Value =
        serde_json::from_slice(&AnnotationScheme::general_pathology_v1().to_json().unwrap())
            .unwrap();
    scheme_json["finding_sites"] = serde_json::json!([{
        "code_value": "91723000",
        "coding_scheme_designator": "SCT",
        "code_meaning": "Anatomical structure"
    }]);
    let scheme = AnnotationScheme::from_json(&serde_json::to_vec(&scheme_json).unwrap()).unwrap();
    let site = scheme.finding_sites()[0].clone();
    let mut document = WorkspaceDocument::new(
        ViewerSourceIdentity::new(74, 0, 0, 0, 0, 0, (256, 256)),
        scheme,
    )
    .unwrap();
    let layer = document.vector_layers()[0].id();
    let plain = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(10.0, 10.0, 5.0)]),
        )
        .unwrap();
    let with_site = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(20.0, 10.0, 5.0)]),
        )
        .unwrap();
    document
        .set_object_finding_site(with_site, Some(&site))
        .unwrap();
    let external =
        ExternalLayerReference::new("CellViT", ExternalLayerKind::DicomAnn, None, None, 3);
    let external_id = external.id();
    document.add_external_layer(external).unwrap();
    for (index, source_frame) in [
        SourceFrameContext::new(Some("A".into()), None, None, None),
        SourceFrameContext::new(Some("B".into()), None, None, None),
    ]
    .into_iter()
    .enumerate()
    {
        document
            .promote_vector_finding(
                layer,
                "neoplasm",
                VectorFindingGeometry::regions(vec![square(30.0 + index as f64 * 10.0, 10.0, 5.0)]),
                ExternalPromotionSource::new(
                    external_id,
                    format!("cell-{index}"),
                    None,
                    source_frame,
                ),
            )
            .unwrap();
    }

    let export = document
        .export_automatic_bulk_ann(&context, automatic_algorithm())
        .unwrap();

    assert_eq!(export.document().groups().len(), 4);
    assert!(export.document().groups()[0].anatomic_regions().is_empty());
    assert_eq!(export.document().groups()[1].anatomic_regions(), &[site]);
    assert_eq!(
        export.document().groups()[2].referenced_optical_paths(),
        &["A"]
    );
    assert_eq!(
        export.document().groups()[3].referenced_optical_paths(),
        &["B"]
    );
    assert_eq!(export.annotation_locations()[0].object_id(), plain);
}

#[test]
fn ann_exports_reject_every_explicit_plane_axis_including_zero() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi_with_optical_paths(&source, 256, 256, 64, 64, &["A"]);
    let context = DicomAnnotationContext::from_source(&source).unwrap();

    for source_frame in [
        SourceFrameContext::new(Some("A".into()), Some(0), None, None),
        SourceFrameContext::new(Some("A".into()), None, Some(0), None),
        SourceFrameContext::new(Some("A".into()), None, None, Some(0)),
    ] {
        let mut document = workspace();
        let layer = document.vector_layers()[0].id();
        let external =
            ExternalLayerReference::new("source", ExternalLayerKind::DicomAnn, None, None, 1);
        let external_id = external.id();
        document.add_external_layer(external).unwrap();
        document
            .promote_vector_finding(
                layer,
                "neoplasm",
                VectorFindingGeometry::regions(vec![square(10.0, 10.0, 5.0)]),
                ExternalPromotionSource::new(external_id, "finding", None, source_frame),
            )
            .unwrap();

        for error in [
            document.export_ann(&context).unwrap_err(),
            document
                .export_automatic_bulk_ann(&context, automatic_algorithm())
                .unwrap_err(),
        ] {
            assert!(error.to_string().contains("2D ANN"));
            assert!(error.to_string().contains("source axes"));
        }
    }
}

#[test]
fn ann_exports_reject_unknown_optical_paths_before_annotation_construction() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi_with_optical_paths(&source, 256, 256, 64, 64, &["A"]);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let mut document = workspace();
    let layer = document.vector_layers()[0].id();
    let external =
        ExternalLayerReference::new("source", ExternalLayerKind::DicomAnn, None, None, 1);
    let external_id = external.id();
    document.add_external_layer(external).unwrap();
    document
        .promote_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(10.0, 10.0, 5.0)]),
            ExternalPromotionSource::new(
                external_id,
                "finding",
                None,
                SourceFrameContext::new(Some("UNKNOWN".into()), None, None, None),
            ),
        )
        .unwrap();

    let error = document.export_ann(&context).unwrap_err();
    assert!(error.to_string().contains("UNKNOWN"));
    assert!(error.to_string().contains("source WSI"));
}

#[test]
fn multi_optical_ann_group_is_readable_but_has_a_precise_promotion_block_reason() {
    let group = crate::AnnotationGroup::points(
        "cells",
        DicomCode::new("49755003", "SCT", "Morphologically abnormal structure").unwrap(),
        DicomCode::new("4421005", "SCT", "Cell structure").unwrap(),
        [1, 2, 3],
        vec![Point2::new(1.0, 1.0)],
    )
    .unwrap()
    .with_referenced_optical_paths(vec!["A".into(), "B".into()])
    .unwrap();

    let error = SourceFrameContext::from_ann_group(&group).unwrap_err();
    assert!(error.to_string().contains("2 optical paths"));
    assert!(error.to_string().contains("remains read-only"));
    assert!(error.to_string().contains("lose applicability"));
}

#[test]
fn compatibility_ann_uses_the_same_source_context_preflight() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi_with_optical_paths(&source, 256, 256, 64, 64, &["A"]);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let mut document = WorkspaceDocument::new(
        ViewerSourceIdentity::new(75, 0, 0, 0, 0, 0, (256, 256)),
        AnnotationScheme::tumor_mask_compatibility_v1(),
    )
    .unwrap();
    let external =
        ExternalLayerReference::new("source", ExternalLayerKind::DicomAnn, None, None, 1);
    let external_id = external.id();
    document.add_external_layer(external).unwrap();
    document
        .promote_vector_finding(
            document.vector_layers()[0].id(),
            "cell",
            VectorFindingGeometry::Point(Point2::new(10.0, 10.0)),
            ExternalPromotionSource::new(
                external_id,
                "cell",
                None,
                SourceFrameContext::new(Some("A".into()), Some(0), None, None),
            ),
        )
        .unwrap();

    let error = document
        .export_tumor_mask_compatibility_ann(&context)
        .unwrap_err();
    assert!(error.to_string().contains("2D ANN"));
    assert!(error.to_string().contains("source axes"));
}

#[test]
fn seg_export_rejects_any_nondefault_source_context() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi_with_optical_paths(&source, 256, 256, 64, 64, &["A"]);
    let context = DicomAnnotationContext::from_source(&source).unwrap();

    for source_frame in [
        SourceFrameContext::new(Some("A".into()), None, None, None),
        SourceFrameContext::new(None, Some(0), None, None),
        SourceFrameContext::new(None, None, Some(0), None),
        SourceFrameContext::new(None, None, None, Some(0)),
    ] {
        let mut document = workspace();
        let layer = document.vector_layers()[0].id();
        let external =
            ExternalLayerReference::new("source", ExternalLayerKind::DicomAnn, None, None, 1);
        let external_id = external.id();
        document.add_external_layer(external).unwrap();
        document
            .promote_vector_finding(
                layer,
                "neoplasm",
                VectorFindingGeometry::regions(vec![square(10.0, 10.0, 5.0)]),
                ExternalPromotionSource::new(external_id, "finding", None, source_frame),
            )
            .unwrap();

        let error = document
            .export_seg(&context, VectorSegmentationPolicy::Rasterize)
            .unwrap_err();
        assert!(error.to_string().contains("SEG"));
        assert!(error.to_string().contains("source context"));
    }
}

#[test]
fn automatic_bulk_ann_rejects_per_object_names_and_comments() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    write_source_wsi(&source, 256, 256, 64, 64);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    for comment in [false, true] {
        let mut document = workspace();
        let finding = document
            .add_vector_finding(
                document.vector_layers()[0].id(),
                "neoplasm",
                VectorFindingGeometry::regions(vec![square(10.0, 10.0, 10.0)]),
            )
            .unwrap();
        if comment {
            document
                .set_object_comment(finding, Some("reviewed"))
                .unwrap();
        } else {
            document.set_object_name(finding, Some("cell 1")).unwrap();
        }

        let error = document
            .export_automatic_bulk_ann(&context, automatic_algorithm())
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("bulk ANN cannot preserve per-annotation name or comment"));
    }
}

#[test]
fn automatic_bulk_ann_round_trips_7266_polygons_in_one_group() {
    const POLYGON_COUNT: usize = 7_266;

    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dcm");
    let ann_path = temp.path().join("bulk-ann.dcm");
    write_source_wsi(&source, 256, 256, 64, 64);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let mut document = workspace();
    let layer = document.vector_layers()[0].id();
    for index in 0..POLYGON_COUNT {
        let x = (index % 200) as f64;
        let y = ((index / 200) % 200) as f64;
        document
            .add_vector_finding(
                layer,
                "neoplasm",
                VectorFindingGeometry::regions(vec![square(x, y, 1.0)]),
            )
            .unwrap();
    }

    let export = document
        .export_automatic_bulk_ann(&context, automatic_algorithm())
        .unwrap();
    assert_eq!(export.document().groups().len(), 1);
    assert_eq!(
        export.document().groups()[0].annotation_count(),
        POLYGON_COUNT
    );
    assert_eq!(export.annotation_locations().len(), POLYGON_COUNT);
    export.document().write_ann(&ann_path).unwrap();

    let restored = AnnotationDocument::read_ann(&ann_path, &context).unwrap();
    assert_eq!(restored.groups().len(), 1);
    assert_eq!(restored.groups()[0].annotation_count(), POLYGON_COUNT);
    assert_eq!(
        restored.groups()[0].polygon_annotations(),
        export.document().groups()[0].polygon_annotations()
    );
    let restored_group = &restored.groups()[0];
    for location in export.annotation_locations() {
        assert_eq!(location.group_uid(), restored_group.uid());
        let start = usize::try_from(location.first_annotation_index().get() - 1).unwrap();
        let end = start + usize::try_from(location.annotation_count().get()).unwrap();
        assert!(end <= restored_group.annotation_count());
        assert_eq!(end - start, 1);
    }
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
