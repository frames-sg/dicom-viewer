use std::collections::HashSet;
use std::sync::Arc;

use dicom_viewer_core::{
    AnnotationDocument, AnnotationGroup, AnnotationScheme, DicomAnnotationContext,
    LinearMeasurementSpec, MeasurementReportSemantics, Point2, SegmentationDocument,
    SegmentationSegment, StructuredReportDocument, TrackingIdentity, VectorFindingGeometry,
    ViewerSourceIdentity,
};
use uuid::Uuid;

use super::{
    ActiveTool, DraftInteraction, DraftResolution, SchemeInstallOutcome, SchemeLibrary,
    ToolTransitionOutcome, WorkspaceRuntime, WorkspaceSpatialIndex,
};
use crate::app::report::ReportSession;

fn source() -> ViewerSourceIdentity {
    ViewerSourceIdentity::new(9, 0, 0, 0, 0, 0, (5_000, 4_000))
}

fn square(x: f64) -> VectorFindingGeometry {
    VectorFindingGeometry::regions(vec![vec![
        Point2::new(x, 10.0),
        Point2::new(x + 20.0, 10.0),
        Point2::new(x + 20.0, 30.0),
        Point2::new(x, 30.0),
    ]])
}

#[test]
fn history_undoes_and_redoes_whole_commands_and_new_edits_invalidate_redo() {
    let mut runtime = WorkspaceRuntime::with_history_limits(
        source(),
        AnnotationScheme::general_pathology_v1(),
        500,
        256 * 1024 * 1024,
    )
    .unwrap();
    let layer = runtime.document().vector_layers()[0].id();

    runtime
        .edit("Add finding", |document| {
            document.add_vector_finding(layer, "neoplasm", square(10.0))
        })
        .unwrap();
    assert_eq!(runtime.document().object_count(), 1);
    assert!(runtime.can_undo());
    assert!(!runtime.can_redo());

    assert!(runtime.undo());
    assert_eq!(runtime.document().object_count(), 0);
    assert!(runtime.can_redo());
    assert!(runtime.redo());
    assert_eq!(runtime.document().object_count(), 1);

    assert!(runtime.undo());
    runtime
        .edit("Add replacement", |document| {
            document.add_vector_finding(layer, "necrosis", square(100.0))
        })
        .unwrap();
    assert!(!runtime.can_redo());
    assert_eq!(
        runtime
            .document()
            .vector_findings()
            .next()
            .unwrap()
            .class_id(),
        "necrosis"
    );
}

#[test]
fn history_evicts_oldest_entries_by_count_and_reports_truncation() {
    let mut runtime = WorkspaceRuntime::with_history_limits(
        source(),
        AnnotationScheme::general_pathology_v1(),
        2,
        usize::MAX,
    )
    .unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    for index in 0..3 {
        runtime
            .edit("Add finding", |document| {
                document.add_vector_finding(layer, "neoplasm", square(index as f64 * 50.0))
            })
            .unwrap();
    }
    assert!(runtime.history_truncated());
    assert!(runtime.undo());
    assert!(runtime.undo());
    assert!(!runtime.undo());
    assert_eq!(runtime.document().object_count(), 1);
}

#[test]
fn history_evicts_by_retained_byte_limit_as_well_as_command_count() {
    let mut runtime = WorkspaceRuntime::with_history_limits(
        source(),
        AnnotationScheme::general_pathology_v1(),
        500,
        1,
    )
    .unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    runtime
        .edit("Large command", |document| {
            document.add_vector_finding(layer, "neoplasm", square(10.0))
        })
        .unwrap();

    assert!(runtime.history_truncated());
    assert!(!runtime.can_undo());
    assert_eq!(runtime.document().object_count(), 1);
}

#[test]
fn unfinished_polygon_blocks_tool_transition_until_explicit_resolution() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    runtime.set_active_tool(ActiveTool::Polygon).unwrap();
    runtime.set_draft(DraftInteraction::vector_polygon(
        layer,
        "neoplasm",
        vec![Point2::new(10.0, 10.0), Point2::new(20.0, 10.0)],
    ));

    assert_eq!(
        runtime.request_tool(ActiveTool::Brush),
        ToolTransitionOutcome::BlockedByDraft
    );
    assert_eq!(runtime.active_tool(), ActiveTool::Polygon);
    assert!(runtime.draft().is_some());

    runtime
        .resolve_draft_transition(DraftResolution::Resume)
        .unwrap();
    assert_eq!(runtime.active_tool(), ActiveTool::Polygon);
    assert!(runtime.draft().is_some());

    assert_eq!(
        runtime.request_tool(ActiveTool::Select),
        ToolTransitionOutcome::BlockedByDraft
    );
    runtime
        .resolve_draft_transition(DraftResolution::Discard)
        .unwrap();
    assert_eq!(runtime.active_tool(), ActiveTool::Select);
    assert!(runtime.draft().is_none());
}

#[test]
fn escape_cancellation_is_recoverable_through_local_draft_undo() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    let points = vec![
        Point2::new(10.0, 10.0),
        Point2::new(20.0, 10.0),
        Point2::new(20.0, 20.0),
    ];
    runtime.set_draft(DraftInteraction::vector_polygon(
        layer,
        "neoplasm",
        points.clone(),
    ));

    assert!(runtime.cancel_draft_step());
    assert_eq!(runtime.draft().unwrap().points(), &points[..2]);
    assert!(runtime.undo_draft_cancel());
    assert_eq!(runtime.draft().unwrap().points(), points);

    for _ in 0..3 {
        assert!(runtime.cancel_draft_step());
    }
    assert!(runtime.draft().is_none());
    assert!(runtime.undo_draft_cancel());
    assert_eq!(runtime.draft().unwrap().points(), &points[..1]);
}

#[test]
fn brush_activation_cannot_create_or_switch_layers_while_a_polygon_is_unfinished() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    runtime.set_draft(DraftInteraction::vector_polygon(
        layer,
        "neoplasm",
        vec![Point2::new(10.0, 10.0), Point2::new(20.0, 10.0)],
    ));

    assert!(runtime.ensure_segmentation_layer().is_err());
    assert!(runtime.document().segmentation_layers().is_empty());
    assert_eq!(
        runtime.editing_representation(),
        super::EditingRepresentation::Vector
    );
}

#[test]
fn scheme_migration_is_blocked_while_a_polygon_is_unfinished() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let original_digest = runtime.document().scheme().content_digest().to_owned();
    let layer = runtime.document().vector_layers()[0].id();
    runtime.set_draft(DraftInteraction::vector_polygon(
        layer,
        "neoplasm",
        vec![Point2::new(10.0, 10.0), Point2::new(20.0, 10.0)],
    ));

    assert!(runtime
        .migrate_scheme(
            AnnotationScheme::tumor_mask_compatibility_v1(),
            &std::collections::BTreeMap::new(),
        )
        .is_err());
    assert_eq!(
        runtime.document().scheme().content_digest(),
        original_digest
    );
    assert!(runtime.draft().is_some());
}

#[test]
fn presentation_changes_are_persisted_but_bypass_document_undo() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    let before_revision = runtime.document().revision();
    runtime.set_layer_visibility(layer, false).unwrap();
    assert!(!runtime.document().presentation().layer(layer).visible);
    assert!(runtime.document().revision() > before_revision);
    assert!(!runtime.can_undo());
}

#[test]
fn scheme_library_deduplicates_content_rejects_conflicts_and_protects_builtins() {
    let mut library = SchemeLibrary::with_builtins();
    let scheme = AnnotationScheme::general_pathology_v1();
    assert_eq!(
        library.install_json(&scheme.to_json().unwrap()).unwrap(),
        SchemeInstallOutcome::AlreadyInstalled
    );

    let mut conflicting: serde_json::Value =
        serde_json::from_slice(&scheme.to_json().unwrap()).unwrap();
    conflicting["display_name"] = "Conflicting name".into();
    assert!(library
        .install_json(&serde_json::to_vec(&conflicting).unwrap())
        .is_err());
    assert!(library
        .remove_project_scheme(scheme.id(), scheme.version(), &HashSet::new())
        .is_err());

    let mut project: serde_json::Value =
        serde_json::from_slice(&scheme.to_json().unwrap()).unwrap();
    project["scheme_id"] = "org.example.project".into();
    let project = serde_json::to_vec(&project).unwrap();
    assert_eq!(
        library.install_json(&project).unwrap(),
        SchemeInstallOutcome::Installed
    );
    let installed = AnnotationScheme::from_json(&project).unwrap();
    let references = HashSet::from([installed.content_digest().to_owned()]);
    assert!(library
        .remove_project_scheme(installed.id(), installed.version(), &references)
        .is_err());
    library
        .remove_project_scheme(installed.id(), installed.version(), &HashSet::new())
        .unwrap();
}

#[test]
fn project_schemes_are_durable_and_removal_is_reference_protected() {
    let directory = tempfile::tempdir().unwrap();
    let mut library = SchemeLibrary::load_or_builtins(directory.path().to_path_buf()).unwrap();
    let mut project: serde_json::Value =
        serde_json::from_slice(&AnnotationScheme::general_pathology_v1().to_json().unwrap())
            .unwrap();
    project["scheme_id"] = "org.example.durable".into();
    let bytes = serde_json::to_vec(&project).unwrap();
    let installed = AnnotationScheme::from_json(&bytes).unwrap();
    library.install_json(&bytes).unwrap();
    drop(library);

    let mut restored = SchemeLibrary::load_or_builtins(directory.path().to_path_buf()).unwrap();
    assert!(restored.get(installed.id(), installed.version()).is_some());
    assert!(restored
        .remove_project_scheme(
            installed.id(),
            installed.version(),
            &HashSet::from([installed.content_digest().to_owned()]),
        )
        .is_err());
    restored
        .remove_project_scheme(installed.id(), installed.version(), &HashSet::new())
        .unwrap();
    drop(restored);

    let reloaded = SchemeLibrary::load_or_builtins(directory.path().to_path_buf()).unwrap();
    assert!(reloaded.get(installed.id(), installed.version()).is_none());
}

#[test]
fn spatial_index_queries_only_viewport_intersecting_workspace_objects() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    let near = runtime
        .edit("Near", |document| {
            document.add_vector_finding(layer, "neoplasm", square(10.0))
        })
        .unwrap();
    let far = runtime
        .edit("Far", |document| {
            document.add_vector_finding(layer, "neoplasm", square(1_000.0))
        })
        .unwrap();
    let index = WorkspaceSpatialIndex::build(runtime.document()).unwrap();
    let found = index.query([0.0, 0.0, 100.0, 100.0], 100);
    assert!(found.contains(&near));
    assert!(!found.contains(&far));
}

#[test]
fn overlay_query_aggregates_all_visible_points_and_never_drops_selection() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    for index in 0..20 {
        runtime
            .edit("Point", |document| {
                document.add_vector_finding(
                    layer,
                    "cell",
                    VectorFindingGeometry::Point(Point2::new(10.0 + index as f64, 20.0)),
                )
            })
            .unwrap();
    }
    let selected = runtime
        .edit("Selected region", |document| {
            document.add_vector_finding(layer, "neoplasm", square(40.0))
        })
        .unwrap();
    let index = WorkspaceSpatialIndex::build(runtime.document()).unwrap();
    let query = index.query_for_overlay([0.0, 0.0, 200.0, 200.0], 0, &HashSet::from([selected]));

    assert_eq!(query.points().len(), 20);
    assert_eq!(query.detailed_ids(), &[selected]);
}

#[test]
fn immutable_history_snapshots_share_coordinate_buffers() {
    let geometry = square(10.0);
    let components = match &geometry {
        VectorFindingGeometry::Regions(components) => components,
        VectorFindingGeometry::Point(_) => unreachable!(),
    };
    let coordinates = Arc::clone(&components[0]);
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    runtime
        .edit("Add", |document| {
            document.add_vector_finding(layer, "neoplasm", geometry)
        })
        .unwrap();
    assert!(Arc::strong_count(&coordinates) >= 2);
    assert!(runtime.undo());
    assert!(runtime.redo());
}

#[test]
fn selections_are_object_level_and_support_multiple_independent_findings() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    runtime.select_only(first);
    runtime.toggle_selection(second);
    assert_eq!(runtime.selection().len(), 2);
    runtime.toggle_selection(first);
    assert_eq!(runtime.selection(), &HashSet::from([second]));
}

#[test]
fn vertex_drag_is_one_command_and_lost_capture_can_restore_original_geometry() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    let finding = runtime
        .edit("Add", |document| {
            document.add_vector_finding(layer, "neoplasm", square(10.0))
        })
        .unwrap();
    runtime.select_only(finding);
    assert!(runtime.begin_handle_drag(Point2::new(10.0, 10.0), 3.0));
    runtime
        .update_handle_drag(Point2::new(5.0, 10.0), |_, _| None)
        .unwrap();
    assert!(runtime.finish_handle_drag());
    assert!(runtime.undo());
    let VectorFindingGeometry::Regions(components) =
        runtime.document().finding(finding).unwrap().geometry()
    else {
        unreachable!()
    };
    assert_eq!(components[0][0], Point2::new(10.0, 10.0));

    assert!(runtime.begin_handle_drag(Point2::new(10.0, 10.0), 3.0));
    runtime
        .update_handle_drag(Point2::new(6.0, 10.0), |_, _| None)
        .unwrap();
    assert!(runtime.cancel_handle_drag());
    let VectorFindingGeometry::Regions(components) =
        runtime.document().finding(finding).unwrap().geometry()
    else {
        unreachable!()
    };
    assert_eq!(components[0][0], Point2::new(10.0, 10.0));
}

#[test]
fn locked_layers_block_object_edits_but_not_selection_or_presentation() {
    let mut runtime =
        WorkspaceRuntime::new(source(), AnnotationScheme::general_pathology_v1()).unwrap();
    let layer = runtime.document().vector_layers()[0].id();
    let finding = runtime
        .edit("Add", |document| {
            document.add_vector_finding(layer, "neoplasm", square(10.0))
        })
        .unwrap();
    let mut presentation = runtime.document().presentation().layer(layer);
    presentation.locked = true;
    runtime
        .set_layer_presentation_without_history(layer, presentation)
        .unwrap();
    runtime.select_only(finding);
    assert!(runtime.delete_selection().is_err());
    assert!(runtime.reclassify_selection("necrosis").is_err());
    assert!(!runtime.begin_handle_drag(Point2::new(10.0, 10.0), 3.0));
    runtime.set_active_tool(ActiveTool::Point).unwrap();
    assert!(runtime.add_point_finding(Point2::new(50.0, 50.0)).is_err());
    assert!(runtime.document().finding(finding).is_some());
    runtime.set_layer_visibility(layer, false).unwrap();
    assert!(!runtime.document().presentation().layer(layer).visible);
}

#[test]
fn imported_ann_requires_explicit_complete_mapping_before_atomic_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    crate::app::tests::write_source_wsi(&source_path);
    let context = DicomAnnotationContext::from_source(&source_path).unwrap();
    let scheme = AnnotationScheme::general_pathology_v1();
    let neoplasm = scheme.class("neoplasm").unwrap();
    let group = AnnotationGroup::polygons(
        "Imported neoplasm",
        neoplasm.category().clone(),
        neoplasm.property_type().clone(),
        neoplasm.recommended_display_cielab(),
        vec![vec![
            Point2::new(1.0, 1.0),
            Point2::new(6.0, 1.0),
            Point2::new(6.0, 6.0),
            Point2::new(1.0, 6.0),
        ]],
    )
    .unwrap();
    let source_object_id = format!("{}:1", group.uid());
    let ann = AnnotationDocument::new(context, vec![group]).unwrap();
    let mut runtime = WorkspaceRuntime::new(
        ViewerSourceIdentity::new(9, 0, 0, 0, 0, 0, (16, 16)),
        scheme,
    )
    .unwrap();
    let external = runtime
        .add_external_annotation("Imported ANN", Some(source_path), ann)
        .unwrap();

    let classes = runtime.external_classes(external).unwrap();
    assert_eq!(classes.len(), 1);
    assert_eq!(
        classes[0].exact_scheme_class_id.as_deref(),
        Some("neoplasm")
    );
    assert!(runtime.make_external_layer_editable(external).is_err());

    runtime
        .set_external_class_mapping(external, &classes[0].key, "neoplasm")
        .unwrap();
    let promoted = runtime
        .promote_external_object(external, &source_object_id)
        .unwrap();
    assert_eq!(
        runtime.document().finding(promoted).unwrap().class_id(),
        "neoplasm"
    );
    assert_eq!(runtime.document().scheme().classes().len(), 8);
    assert!(runtime
        .promote_external_object(external, &source_object_id)
        .is_err());
}

#[test]
fn external_layer_removal_is_undoable_and_retains_the_shared_payload() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    crate::app::tests::write_source_wsi(&source_path);
    let context = DicomAnnotationContext::from_source(&source_path).unwrap();
    let scheme = AnnotationScheme::general_pathology_v1();
    let neoplasm = scheme.class("neoplasm").unwrap();
    let group = AnnotationGroup::polygons(
        "Imported neoplasm",
        neoplasm.category().clone(),
        neoplasm.property_type().clone(),
        neoplasm.recommended_display_cielab(),
        vec![vec![
            Point2::new(1.0, 1.0),
            Point2::new(6.0, 1.0),
            Point2::new(6.0, 6.0),
            Point2::new(1.0, 6.0),
        ]],
    )
    .unwrap();
    let ann = AnnotationDocument::new(context, vec![group]).unwrap();
    let mut runtime = WorkspaceRuntime::new(
        ViewerSourceIdentity::new(9, 0, 0, 0, 0, 0, (16, 16)),
        scheme,
    )
    .unwrap();
    let external = runtime
        .add_external_annotation("Imported ANN", Some(source_path), ann)
        .unwrap();
    let before = match runtime.external_payload(external).unwrap() {
        super::ExternalLayerPayload::Annotation(payload) => Arc::clone(payload),
        _ => unreachable!(),
    };

    runtime.remove_external_layer(external).unwrap();
    assert!(runtime
        .document()
        .external_layers()
        .iter()
        .all(|layer| layer.id() != external));
    assert!(runtime.undo());
    assert!(runtime
        .document()
        .external_layers()
        .iter()
        .any(|layer| layer.id() == external));
    let restored = match runtime.external_payload(external).unwrap() {
        super::ExternalLayerPayload::Annotation(payload) => payload,
        _ => unreachable!(),
    };
    assert!(Arc::ptr_eq(&before, restored));
}

#[test]
fn loading_a_discovered_sidecar_hydrates_its_stub_instead_of_duplicating_the_layer() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    let sidecar_path = directory.path().join("annotations.dcm");
    crate::app::tests::write_source_wsi(&source_path);
    let context = DicomAnnotationContext::from_source(&source_path).unwrap();
    let scheme = AnnotationScheme::general_pathology_v1();
    let neoplasm = scheme.class("neoplasm").unwrap();
    let group = AnnotationGroup::polygons(
        "Imported neoplasm",
        neoplasm.category().clone(),
        neoplasm.property_type().clone(),
        neoplasm.recommended_display_cielab(),
        vec![vec![
            Point2::new(1.0, 1.0),
            Point2::new(6.0, 1.0),
            Point2::new(6.0, 6.0),
            Point2::new(1.0, 6.0),
        ]],
    )
    .unwrap();
    let ann = AnnotationDocument::new(context, vec![group]).unwrap();
    let mut runtime = WorkspaceRuntime::new(
        ViewerSourceIdentity::new(9, 0, 0, 0, 0, 0, (16, 16)),
        scheme,
    )
    .unwrap();
    let stub = runtime
        .ensure_discovered_external_stub(
            "annotations.dcm",
            dicom_viewer_core::ExternalLayerKind::DicomAnn,
            sidecar_path.clone(),
        )
        .unwrap();

    let loaded = runtime
        .add_external_annotation("annotations.dcm", Some(sidecar_path), ann)
        .unwrap();

    assert_eq!(loaded, stub);
    assert_eq!(runtime.document().external_layers().len(), 1);
    assert_eq!(
        runtime.document().external_layers()[0].source_object_count(),
        1
    );
    assert!(matches!(
        runtime.external_payload(stub),
        Some(super::ExternalLayerPayload::Annotation(_))
    ));
}

#[test]
fn imported_seg_conversion_keeps_same_class_segments_independent_and_tracks_sources() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    crate::app::tests::write_source_wsi(&source_path);
    let context = DicomAnnotationContext::from_source(&source_path).unwrap();
    let scheme = AnnotationScheme::general_pathology_v1();
    let neoplasm = scheme.class("neoplasm").unwrap();
    let make_segment = |offset, id: &str, uid: &str| {
        SegmentationSegment::new(
            "Imported neoplasm",
            neoplasm.category().clone(),
            neoplasm.property_type().clone(),
            neoplasm.recommended_display_cielab(),
            vec![vec![
                Point2::new(offset, 1.0),
                Point2::new(offset + 3.0, 1.0),
                Point2::new(offset + 3.0, 4.0),
                Point2::new(offset, 4.0),
            ]],
            Vec::new(),
        )
        .unwrap()
        .with_tracking(id, uid)
        .unwrap()
    };
    let segmentation = SegmentationDocument::binary(
        context,
        vec![
            make_segment(1.0, "SOURCE-SEG-1", "2.25.101"),
            make_segment(4.5, "SOURCE-SEG-2", "2.25.102"),
        ],
    )
    .unwrap();
    let mut runtime =
        WorkspaceRuntime::new(ViewerSourceIdentity::new(9, 0, 0, 0, 0, 0, (8, 8)), scheme).unwrap();
    let (external, diagnostics) = runtime
        .add_external_segmentation("Imported SEG", Some(source_path), segmentation)
        .unwrap();
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == "SEG_TRACKING_ID_NOT_REPRESENTABLE"));
    let class = runtime.external_classes(external).unwrap().remove(0);
    runtime
        .set_external_class_mapping(external, &class.key, "neoplasm")
        .unwrap();
    let promoted = runtime.make_external_layer_editable(external).unwrap();

    assert_eq!(promoted.len(), 2);
    assert_eq!(runtime.document().segments().count(), 2);
    let tracking = runtime
        .document()
        .segments()
        .map(|segment| segment.tracking().clone())
        .collect::<Vec<_>>();
    assert!(tracking.contains(&TrackingIdentity::new("SOURCE-SEG-1", "2.25.101").unwrap()));
    assert!(tracking.contains(&TrackingIdentity::new("SOURCE-SEG-2", "2.25.102").unwrap()));
}

#[test]
fn compatible_two_point_sr_can_be_promoted_after_class_mapping() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    crate::app::tests::write_source_wsi(&source_path);
    let context = DicomAnnotationContext::from_source(&source_path).unwrap();
    let scheme = AnnotationScheme::general_pathology_v1();
    let neoplasm = scheme.class("neoplasm").unwrap();
    let tracking = TrackingIdentity::new("SOURCE-RULER", "2.25.9001").unwrap();
    let spec = LinearMeasurementSpec::new(
        tracking.clone(),
        neoplasm.category().clone(),
        neoplasm.property_type().clone(),
        Point2::new(1.0, 1.0),
        Point2::new(5.0, 1.0),
    )
    .unwrap();
    let report = StructuredReportDocument::from_linear_measurements(
        context,
        &MeasurementReportSemantics::pathology_v1(),
        &[spec],
    )
    .unwrap();
    let mut runtime = WorkspaceRuntime::new(
        ViewerSourceIdentity::new(9, 0, 0, 0, 0, 0, (16, 16)),
        scheme,
    )
    .unwrap();
    let external = runtime
        .add_external_report(
            "Imported SR",
            Some(source_path),
            ReportSession::from_document(report).unwrap(),
        )
        .unwrap();
    let class = runtime.external_classes(external).unwrap().remove(0);
    runtime
        .set_external_class_mapping(external, &class.key, "neoplasm")
        .unwrap();
    let source_object = runtime.external_objects(external).unwrap().remove(0);
    let promoted = runtime
        .promote_external_object(external, &source_object.source_object_id)
        .unwrap();

    let measurement = runtime.document().measurement(promoted).unwrap();
    assert_eq!(measurement.tracking(), &tracking);
    assert!(measurement
        .physical_length_mm()
        .is_some_and(|value| value > 0.0));
}
