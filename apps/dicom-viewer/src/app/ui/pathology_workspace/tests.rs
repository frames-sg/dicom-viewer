use super::*;
use crate::app::tests::run_ui;
use dicom_viewer_core::{AnnotationScheme, ViewerSourceIdentity};

fn runtime() -> WorkspaceRuntime {
    WorkspaceRuntime::new(
        ViewerSourceIdentity::new(1, 0, 0, 0, 0, 0, (1_000, 1_000)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap()
}

#[test]
fn pathology_workspace_renders_as_one_dense_panel_and_tool_rail() {
    let mut runtime = runtime();
    let output = run_ui(|ui| {
        let _ = show_tool_rail(ui, &mut runtime);
        show_pathology_workspace_panel(ui, &mut runtime);
    });
    assert!(!output.shapes.is_empty());
}

#[test]
fn pathology_panel_stays_closed_until_the_document_has_a_tracked_object() {
    let mut runtime = runtime();
    let mut panel_rendered = true;
    let _ = run_ui(|ui| {
        panel_rendered = show_populated_pathology_workspace_panel(ui, &mut runtime).is_some();
    });
    assert!(!panel_rendered);

    runtime.set_active_tool(ActiveTool::Point).unwrap();
    runtime
        .add_point_finding(dicom_viewer_core::Point2::new(10.0, 20.0))
        .unwrap();
    let _ = run_ui(|ui| {
        panel_rendered = show_populated_pathology_workspace_panel(ui, &mut runtime).is_some();
    });
    assert!(panel_rendered);
}

#[test]
fn common_palette_filters_classes_by_tool_geometry() {
    let mut runtime = runtime();
    runtime.set_active_tool(ActiveTool::Point).unwrap();
    let point_classes = visible_palette(&runtime);
    assert_eq!(
        point_classes
            .iter()
            .map(|class| class.0.as_str())
            .collect::<Vec<_>>(),
        vec!["cell", "nucleus"]
    );
    runtime.set_active_tool(ActiveTool::Polygon).unwrap();
    assert!(visible_palette(&runtime)
        .iter()
        .all(|class| !matches!(class.0.as_str(), "cell" | "nucleus")));
}

#[test]
fn findings_reuse_unchanged_rows_and_refresh_after_edit_undo_and_study_replacement() {
    use std::sync::Arc;
    let mut runtime = runtime();
    let mut cache = super::findings::FindingRowsCache::default();
    runtime.set_active_tool(ActiveTool::Point).unwrap();
    let id = runtime
        .add_point_finding(dicom_viewer_core::Point2::new(10.0, 20.0))
        .unwrap();
    let first = cache.rows(&runtime);
    let unchanged = cache.rows(&runtime);
    assert!(Arc::ptr_eq(&first, &unchanged));
    runtime.select_only(id);
    runtime.reclassify_selection("nucleus").unwrap();
    let changed = cache.rows(&runtime);
    assert_ne!(changed[0].label, first[0].label);
    assert_eq!(changed[0].id, id);
    assert!(runtime.undo());
    let restored = cache.rows(&runtime);
    assert_eq!(restored[0].label, first[0].label);
    let empty = WorkspaceRuntime::new(
        ViewerSourceIdentity::new(2, 0, 0, 0, 0, 0, (1_000, 1_000)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap();
    assert!(cache.rows(&empty).is_empty());
}

#[test]
#[ignore = "manual CPU characterization; run with --release --ignored --nocapture --test-threads=1"]
fn cpu_workspace_release_characterization() {
    use std::hint::black_box;
    use std::time::Instant;

    if cfg!(debug_assertions) {
        panic!("run this characterization with --release");
    }
    for (points, regions, segments) in [(1_000, 0, 0), (50_000, 20_000, 0), (0, 0, 100)] {
        let mut runtime =
            WorkspaceRuntime::from_document(cpu_audit_document(points, regions, segments)).unwrap();
        let expected = finding_rows(&runtime);
        let mut row_cache = super::findings::FindingRowsCache::default();
        // Characterize stable redraws after the initial frame populated the cache.
        let _ = row_cache.rows(&runtime);
        assert_eq!(expected.len(), points + regions + segments);
        assert!(expected
            .windows(2)
            .all(|pair| pair[0].ordinal < pair[1].ordinal));
        assert!(expected[..points].iter().all(|row| row.metric == "point"));
        assert!(expected[points..points + regions]
            .iter()
            .all(|row| row.metric == "1 region"));
        assert!(expected[points + regions..]
            .iter()
            .all(|row| row.metric == "1 component"));
        let selected = expected.last().unwrap().id;
        runtime.select_only(selected);
        let original = runtime.document_snapshot();
        let original_bounds = runtime.object_bounds(selected);
        assert!(original_bounds.is_some());

        // Each sample starts from the same document. Undo and its index refresh
        // happen outside the measured rename and refresh phases.
        for sample in 0..15 {
            let started = Instant::now();
            let rows = black_box(row_cache.rows(black_box(&runtime)));
            let rows_ms = started.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(rows.len(), expected.len());
            for (actual, expected) in rows.iter().zip(&expected) {
                assert_eq!(
                    (actual.id, actual.ordinal, &actual.label, &actual.metric),
                    (
                        expected.id,
                        expected.ordinal,
                        &expected.label,
                        &expected.metric
                    )
                );
            }
            drop(rows);

            let started = Instant::now();
            runtime
                .set_selected_name(Some("CPU characterization"))
                .unwrap();
            let rename_ms = started.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(runtime.document().object_count(), expected.len());
            assert_eq!(
                runtime.document().object(selected).unwrap().name(),
                Some("CPU characterization")
            );
            assert_eq!(original.object(selected).unwrap().name(), None);

            let started = Instant::now();
            runtime.refresh_spatial_index().unwrap();
            let refresh_ms = started.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(runtime.object_bounds(selected), original_bounds);

            assert!(runtime.undo());
            assert_eq!(runtime.document().object(selected).unwrap().name(), None);
            assert_eq!(runtime.document().revision(), original.revision());
            runtime.refresh_spatial_index().unwrap();
            eprintln!(
                "{}",
                serde_json::json!({
                    "workload": "cpu-workspace", "sample": sample + 1,
                    "points": points, "regions": regions, "region_vertices": 20,
                    "segments": segments, "brush_points": 32,
                    "rows": expected.len(), "rows_ms": rows_ms,
                    "rename_ms": rename_ms, "refresh_ms": refresh_ms,
                })
            );
        }
    }
}

fn cpu_audit_document(
    points: usize,
    regions: usize,
    segments: usize,
) -> dicom_viewer_core::WorkspaceDocument {
    use dicom_viewer_core::{
        Point2, SegmentationPrimitive, VectorFindingGeometry, WorkspaceDocument,
    };

    let mut document = WorkspaceDocument::new(
        ViewerSourceIdentity::new(44, 0, 0, 0, 0, 0, (16_384, 16_384)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap();
    let layer = document.vector_layers()[0].id();
    for index in 0..points {
        document
            .add_vector_finding(
                layer,
                "cell",
                VectorFindingGeometry::Point(Point2::new(
                    (index % 400) as f64 * 40.0 + 10.0,
                    (index / 400) as f64 * 125.0 + 10.0,
                )),
            )
            .unwrap();
    }
    for index in 0..regions {
        let center_x = (index % 200) as f64 * 80.0 + 40.0;
        let center_y = (index / 200) as f64 * 160.0 + 80.0;
        let contour = (0..20)
            .map(|vertex| {
                let angle = std::f64::consts::TAU * vertex as f64 / 20.0;
                Point2::new(center_x + angle.cos() * 20.0, center_y + angle.sin() * 20.0)
            })
            .collect();
        document
            .add_vector_finding(
                layer,
                "neoplasm",
                VectorFindingGeometry::regions(vec![contour]),
            )
            .unwrap();
    }
    if segments > 0 {
        let layer = document.ensure_manual_segmentation_layer();
        for index in 0..segments {
            let x = (index % 10) as f64 * 200.0 + 40.0;
            let y = (index / 10) as f64 * 200.0 + 40.0;
            let centerline = (0..32)
                .map(|point| {
                    Point2::new(
                        x + point as f64 * 3.0,
                        y + (point as f64 * 0.25).sin() * 10.0,
                    )
                })
                .collect();
            document
                .add_segment(
                    layer,
                    "neoplasm",
                    SegmentationPrimitive::brush(SegmentOperation::Add, centerline, 20.0),
                )
                .unwrap();
        }
    }
    document
}
