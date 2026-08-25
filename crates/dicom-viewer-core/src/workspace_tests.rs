use std::collections::BTreeMap;

use serde_json::Value;

use crate::{
    AnnotationScheme, ExternalLayerKind, ExternalLayerReference, ExternalPromotionSource, Point2,
    SegmentEditOutcome, SegmentOperation, SegmentationPrimitive, SegmentationPrimitiveGeometry,
    SourceFrameContext, TrackingIdentity, VectorFindingGeometry, ViewerSourceIdentity,
    WorkspaceDocument,
};

fn source_identity() -> ViewerSourceIdentity {
    ViewerSourceIdentity::new(42, 1, 2, 3, 4, 5, (20_000, 10_000))
}

fn square(x: f64, y: f64, size: f64) -> Vec<Point2> {
    vec![
        Point2::new(x, y),
        Point2::new(x + size, y),
        Point2::new(x + size, y + size),
        Point2::new(x, y + size),
    ]
}

#[test]
fn same_class_vector_findings_keep_identity_and_ordinals_are_never_reused() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let layer = document.vector_layers()[0].id();

    let first = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(10.0, 10.0, 20.0)]),
        )
        .unwrap();
    let second = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(100.0, 100.0, 20.0)]),
        )
        .unwrap();

    assert_ne!(first, second);
    assert_eq!(document.vector_findings().count(), 2);
    let findings = document.vector_findings().collect::<Vec<_>>();
    assert_eq!((findings[0].ordinal(), findings[1].ordinal()), (1, 2));
    assert_ne!(findings[0].tracking().uid(), findings[1].tracking().uid());

    assert!(document.delete_object(first).unwrap());
    let third = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(200.0, 200.0, 20.0)]),
        )
        .unwrap();
    assert_eq!(document.finding(third).unwrap().ordinal(), 3);

    let encoded = document.to_json().unwrap();
    assert!(!encoded
        .windows("class_color_overrides".len())
        .any(|window| window == b"class_color_overrides"));
    let mut legacy: Value = serde_json::from_slice(&encoded).unwrap();
    legacy["presentation"]["class_color_overrides"] = serde_json::json!({"neoplasm": [213, 94, 0]});
    let legacy_restored =
        WorkspaceDocument::from_json(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert_eq!(legacy_restored.object_count(), document.object_count());
    let restored = WorkspaceDocument::from_json(&encoded).unwrap();
    assert_eq!(restored.source_identity(), document.source_identity());
    assert_eq!(
        restored.scheme().content_digest(),
        document.scheme().content_digest()
    );
    assert_eq!(
        restored.finding(third).unwrap().tracking(),
        document.finding(third).unwrap().tracking()
    );
}

#[test]
fn vector_findings_validate_class_geometry_and_simple_polygons() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let layer = document.vector_layers()[0].id();

    assert!(document
        .add_vector_finding(
            layer,
            "cell",
            VectorFindingGeometry::regions(vec![square(0.0, 0.0, 10.0)]),
        )
        .is_err());
    assert!(document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::Point(Point2::new(1.0, 2.0)),
        )
        .is_err());
    assert!(document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![vec![
                Point2::new(0.0, 0.0),
                Point2::new(10.0, 10.0),
                Point2::new(0.0, 10.0),
                Point2::new(10.0, 0.0),
            ]]),
        )
        .is_err());
}

#[test]
fn segmentation_add_and_erase_are_composed_per_segment_not_per_class() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let layer = document.ensure_manual_segmentation_layer();
    let first = document
        .add_segment(
            layer,
            "neoplasm",
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(0.0, 0.0, 100.0)),
        )
        .unwrap();
    let second = document
        .add_segment(
            layer,
            "neoplasm",
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(200.0, 0.0, 50.0)),
        )
        .unwrap();

    assert_eq!(
        document
            .apply_segment_primitive(
                first,
                SegmentationPrimitive::polygon(SegmentOperation::Erase, square(25.0, 25.0, 50.0),),
            )
            .unwrap(),
        SegmentEditOutcome::Applied
    );
    let first_geometry = document.composite_segment(first).unwrap();
    assert_eq!(first_geometry.components().len(), 1);
    assert_eq!(first_geometry.components()[0].holes().len(), 1);
    assert!(document.composite_segment(second).unwrap().components()[0]
        .holes()
        .is_empty());

    let primitive_count = document.segment(first).unwrap().primitives().len();
    assert_eq!(
        document
            .apply_segment_primitive(
                first,
                SegmentationPrimitive::polygon(
                    SegmentOperation::Erase,
                    square(500.0, 500.0, 10.0),
                ),
            )
            .unwrap(),
        SegmentEditOutcome::NoIntersection
    );
    assert_eq!(
        document.segment(first).unwrap().primitives().len(),
        primitive_count
    );

    document
        .apply_segment_primitive(
            first,
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(400.0, 0.0, 25.0)),
        )
        .unwrap();
    assert_eq!(
        document
            .composite_segment(first)
            .unwrap()
            .components()
            .len(),
        2
    );
    assert_eq!(document.segments().count(), 2);
}

#[test]
fn brush_primitives_buffer_the_centerline_in_slide_space() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let layer = document.ensure_manual_segmentation_layer();
    let segment = document
        .add_segment(
            layer,
            "tissue",
            SegmentationPrimitive::brush(
                SegmentOperation::Add,
                vec![Point2::new(10.0, 10.0), Point2::new(30.0, 10.0)],
                10.0,
            ),
        )
        .unwrap();
    let geometry = document.composite_segment(segment).unwrap();
    let bounds = geometry.bounds().unwrap();
    assert!(
        bounds[0] <= 5.05 && bounds[1] <= 5.01,
        "unexpected brush bounds: {bounds:?}"
    );
    assert!(
        bounds[2] >= 34.95 && bounds[3] >= 14.99,
        "unexpected brush bounds: {bounds:?}"
    );
}

#[test]
fn segmentation_primitives_are_clipped_to_slide_bounds() {
    let mut document = WorkspaceDocument::new(
        ViewerSourceIdentity::new(43, 0, 0, 0, 0, 0, (32, 24)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap();
    let layer = document.ensure_manual_segmentation_layer();
    let segment = document
        .add_segment(
            layer,
            "tissue",
            SegmentationPrimitive::polygon(
                SegmentOperation::Add,
                vec![
                    Point2::new(-10.0, -8.0),
                    Point2::new(20.0, -8.0),
                    Point2::new(20.0, 12.0),
                    Point2::new(-10.0, 12.0),
                ],
            ),
        )
        .unwrap();

    let bounds = document
        .composite_segment(segment)
        .unwrap()
        .bounds()
        .unwrap();
    assert_eq!(bounds, [0.0, 0.0, 20.0, 12.0]);
}

#[test]
fn segmentation_composition_matches_a_small_pixel_center_raster_oracle() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let primitives = vec![
        SegmentationPrimitive::polygon(SegmentOperation::Add, square(2.0, 2.0, 26.0)),
        SegmentationPrimitive::polygon(SegmentOperation::Erase, square(8.0, 8.0, 8.0)),
        SegmentationPrimitive::brush(
            SegmentOperation::Add,
            vec![Point2::new(4.2, 22.2), Point2::new(26.2, 22.2)],
            4.2,
        ),
        SegmentationPrimitive::polygon(SegmentOperation::Erase, square(20.0, 18.0, 4.0)),
    ];
    let layer = document.ensure_manual_segmentation_layer();
    let segment = document
        .add_segment(layer, "neoplasm", primitives[0].clone())
        .unwrap();
    for primitive in &primitives[1..] {
        document
            .apply_segment_primitive(segment, primitive.clone())
            .unwrap();
    }
    let computed = document.composite_segment(segment).unwrap();

    for y in 0..32 {
        for x in 0..32 {
            let point = Point2::new(x as f64 + 0.5, y as f64 + 0.5);
            let expected = raster_oracle(&primitives, point);
            let actual = computed.components().iter().any(|component| {
                crate::polygon_contains_point(component.exterior(), point)
                    && !component
                        .holes()
                        .iter()
                        .any(|hole| crate::polygon_contains_point(hole, point))
            });
            assert_eq!(actual, expected, "pixel center ({x}, {y}) differs");
        }
    }
}

fn raster_oracle(primitives: &[SegmentationPrimitive], point: Point2) -> bool {
    let mut included = false;
    for primitive in primitives {
        let hit = match primitive.geometry() {
            SegmentationPrimitiveGeometry::Polygon { points } => {
                crate::polygon_contains_point(points, point)
            }
            SegmentationPrimitiveGeometry::Brush {
                centerline,
                diameter,
            } => {
                let radius_squared = (diameter * 0.5).powi(2);
                if centerline.len() == 1 {
                    squared_distance(point, centerline[0]) <= radius_squared
                } else {
                    centerline.windows(2).any(|segment| {
                        point_segment_squared_distance(point, segment[0], segment[1])
                            <= radius_squared
                    })
                }
            }
        };
        if hit {
            included = primitive.operation() == SegmentOperation::Add;
        }
    }
    included
}

fn squared_distance(left: Point2, right: Point2) -> f64 {
    (left.x - right.x).powi(2) + (left.y - right.y).powi(2)
}

fn point_segment_squared_distance(point: Point2, start: Point2, end: Point2) -> f64 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_squared = dx * dx + dy * dy;
    if length_squared == 0.0 {
        return squared_distance(point, start);
    }
    let projection =
        (((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared).clamp(0.0, 1.0);
    squared_distance(
        point,
        Point2::new(start.x + projection * dx, start.y + projection * dy),
    )
}

#[test]
fn populated_scheme_migration_is_complete_geometry_safe_and_atomic() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let layer = document.vector_layers()[0].id();
    let finding = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(0.0, 0.0, 10.0)]),
        )
        .unwrap();

    let mut json: Value =
        serde_json::from_slice(&AnnotationScheme::general_pathology_v1().to_json().unwrap())
            .unwrap();
    json["scheme_id"] = Value::String("org.frames.general-pathology-renamed".into());
    json["scheme_version"] = Value::from(2);
    json["classes"][1]["id"] = Value::String("tumor-region".into());
    let target = AnnotationScheme::from_json(&serde_json::to_vec(&json).unwrap()).unwrap();

    let original_digest = document.scheme().content_digest().to_owned();
    assert!(document
        .migrate_scheme(target.clone(), &BTreeMap::new())
        .is_err());
    assert_eq!(document.scheme().content_digest(), original_digest);
    assert_eq!(document.finding(finding).unwrap().class_id(), "neoplasm");

    let suggestions = document.suggest_scheme_migration(&target);
    assert_eq!(
        suggestions.get("neoplasm").map(String::as_str),
        Some("tumor-region")
    );
    document.migrate_scheme(target, &suggestions).unwrap();
    assert_eq!(
        document.finding(finding).unwrap().class_id(),
        "tumor-region"
    );
}

#[test]
fn object_level_geometry_reclassification_and_measurement_edits_are_isolated() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let layer = document.vector_layers()[0].id();
    let first = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(10.0, 10.0, 20.0)]),
        )
        .unwrap();
    let second = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(100.0, 100.0, 20.0)]),
        )
        .unwrap();
    document
        .move_vector_vertex(first, 0, 0, Point2::new(5.0, 10.0))
        .unwrap();
    document.reclassify_object(first, "necrosis").unwrap();
    assert_eq!(document.finding(first).unwrap().class_id(), "necrosis");
    assert_eq!(document.finding(second).unwrap().class_id(), "neoplasm");
    let VectorFindingGeometry::Regions(second_geometry) =
        document.finding(second).unwrap().geometry()
    else {
        unreachable!()
    };
    assert_eq!(second_geometry[0][0], Point2::new(100.0, 100.0));
    assert!(document.reclassify_object(first, "cell").is_err());
    assert_eq!(document.finding(first).unwrap().class_id(), "necrosis");

    let measurement = document
        .add_linear_measurement(
            "tissue",
            [Point2::new(0.0, 0.0), Point2::new(3.0, 4.0)],
            Some(0.00125),
        )
        .unwrap();
    document
        .set_measurement_endpoints(
            measurement,
            [Point2::new(0.0, 0.0), Point2::new(6.0, 8.0)],
            Some(0.0025),
        )
        .unwrap();
    assert_eq!(
        document
            .measurement(measurement)
            .unwrap()
            .physical_length_mm(),
        Some(0.0025)
    );
}

#[test]
fn object_names_and_comments_are_individual_and_bounded() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let layer = document.vector_layers()[0].id();
    let first = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(10.0, 10.0, 20.0)]),
        )
        .unwrap();
    let second = document
        .add_vector_finding(
            layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(100.0, 100.0, 20.0)]),
        )
        .unwrap();

    document
        .set_object_name(first, Some("invasive front"))
        .unwrap();
    document
        .set_object_comment(first, Some("review on deeper level"))
        .unwrap();
    assert_eq!(
        document.finding(first).unwrap().name(),
        Some("invasive front")
    );
    assert_eq!(
        document.finding(first).unwrap().comment(),
        Some("review on deeper level")
    );
    assert_eq!(document.finding(second).unwrap().name(), None);
    assert!(document.set_object_name(first, Some("")).is_err());
    assert!(document
        .set_object_comment(first, Some(&"x".repeat(4_097)))
        .is_err());
}

#[test]
fn segment_primitive_vertices_move_without_merging_same_class_segments() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
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
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(100.0, 10.0, 20.0)),
        )
        .unwrap();
    document
        .move_segment_primitive_point(first, 0, 0, Point2::new(5.0, 10.0))
        .unwrap();
    let SegmentationPrimitiveGeometry::Polygon { points } =
        document.segment(first).unwrap().primitives()[0].geometry()
    else {
        unreachable!()
    };
    assert_eq!(points[0], Point2::new(5.0, 10.0));
    let SegmentationPrimitiveGeometry::Polygon { points } =
        document.segment(second).unwrap().primitives()[0].geometry()
    else {
        unreachable!()
    };
    assert_eq!(points[0], Point2::new(100.0, 10.0));
}

#[test]
fn external_class_mapping_never_mutates_scheme_and_promoted_segment_keeps_source_tracking() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let external = ExternalLayerReference::new(
        "model detections",
        ExternalLayerKind::DicomSeg,
        None,
        None,
        1,
    );
    let external_id = document.add_external_layer(external).unwrap();
    let class_count = document.scheme().classes().len();
    document
        .set_external_class_mapping(external_id, "unknown-model-class", "neoplasm")
        .unwrap();
    assert_eq!(document.scheme().classes().len(), class_count);
    assert_eq!(
        document.external_layers()[0]
            .class_mappings()
            .get("unknown-model-class")
            .map(String::as_str),
        Some("neoplasm")
    );
    assert!(document
        .set_external_class_mapping(external_id, "another", "not-installed")
        .is_err());

    let layer = document.ensure_manual_segmentation_layer();
    let tracking = TrackingIdentity::new("SOURCE-SEGMENT-7", "2.25.7").unwrap();
    let promoted = document
        .promote_segment(
            layer,
            "neoplasm",
            vec![SegmentationPrimitive::polygon(
                SegmentOperation::Add,
                square(10.0, 10.0, 20.0),
            )],
            ExternalPromotionSource::new(
                external_id,
                "7",
                Some(tracking.clone()),
                SourceFrameContext::default(),
            ),
        )
        .unwrap();
    assert_eq!(document.segment(promoted).unwrap().tracking(), &tracking);

    let measurement_tracking = TrackingIdentity::new("SOURCE-RULER-8", "2.25.8").unwrap();
    let measurement = document
        .promote_linear_measurement(
            "neoplasm",
            [Point2::new(20.0, 20.0), Point2::new(60.0, 20.0)],
            Some(0.04),
            ExternalPromotionSource::new(
                external_id,
                "8",
                Some(measurement_tracking.clone()),
                SourceFrameContext::default(),
            ),
        )
        .unwrap();
    assert_eq!(
        document.measurement(measurement).unwrap().tracking(),
        &measurement_tracking
    );
}

#[test]
fn external_layer_stub_can_be_hydrated_without_changing_its_identity_or_mappings() {
    let mut document =
        WorkspaceDocument::new(source_identity(), AnnotationScheme::general_pathology_v1())
            .unwrap();
    let external = ExternalLayerReference::new(
        "discovered annotations.dcm",
        ExternalLayerKind::DicomAnn,
        Some("annotations.dcm".into()),
        None,
        0,
    );
    let external_id = document.add_external_layer(external).unwrap();
    document
        .set_external_class_mapping(external_id, "source-neoplasm", "neoplasm")
        .unwrap();

    document
        .hydrate_external_layer(external_id, 17, Some("abc123".into()))
        .unwrap();

    let layer = &document.external_layers()[0];
    assert_eq!(layer.id(), external_id);
    assert_eq!(layer.source_object_count(), 17);
    assert_eq!(layer.source_digest(), Some("abc123"));
    assert_eq!(
        layer
            .class_mappings()
            .get("source-neoplasm")
            .map(String::as_str),
        Some("neoplasm")
    );

    assert!(document
        .hydrate_external_layer(external_id, 99, Some("changed".into()))
        .is_err());
    let layer = &document.external_layers()[0];
    assert_eq!(layer.source_object_count(), 17);
    assert_eq!(layer.source_digest(), Some("abc123"));
}

#[test]
fn finding_site_is_controlled_separately_from_class_for_every_tracked_object() {
    let mut scheme_json: Value =
        serde_json::from_slice(&AnnotationScheme::general_pathology_v1().to_json().unwrap())
            .unwrap();
    scheme_json["scheme_id"] = "org.example.with-sites".into();
    scheme_json["finding_sites"] = serde_json::json!([{
        "code_value": "76752008",
        "coding_scheme_designator": "SCT",
        "code_meaning": "Breast structure"
    }]);
    let scheme = AnnotationScheme::from_json(&serde_json::to_vec(&scheme_json).unwrap()).unwrap();
    let site = scheme.finding_sites()[0].clone();
    let unrelated = crate::DicomCode::new("71836000", "SCT", "Colon structure").unwrap();
    let mut document = WorkspaceDocument::new(source_identity(), scheme).unwrap();
    let vector_layer = document.vector_layers()[0].id();
    let finding = document
        .add_vector_finding(
            vector_layer,
            "neoplasm",
            VectorFindingGeometry::regions(vec![square(10.0, 10.0, 10.0)]),
        )
        .unwrap();
    let segment_layer = document.ensure_manual_segmentation_layer();
    let segment = document
        .add_segment(
            segment_layer,
            "neoplasm",
            SegmentationPrimitive::polygon(SegmentOperation::Add, square(30.0, 30.0, 10.0)),
        )
        .unwrap();
    let ruler = document
        .add_linear_measurement(
            "neoplasm",
            [Point2::new(50.0, 50.0), Point2::new(60.0, 50.0)],
            Some(0.01),
        )
        .unwrap();

    for object in [finding, segment, ruler] {
        document
            .set_object_finding_site(object, Some(&site))
            .unwrap();
        assert!(document
            .set_object_finding_site(object, Some(&unrelated))
            .is_err());
    }
    assert!(document.finding(finding).unwrap().finding_site().is_some());
    assert!(document.segment(segment).unwrap().finding_site().is_some());
    assert!(document
        .measurement(ruler)
        .unwrap()
        .finding_site()
        .is_some());
}
