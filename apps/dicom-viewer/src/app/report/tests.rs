use dicom_viewer_core::{
    AnnotationScheme, DicomAnnotationContext, PathologyAnnotationSet, PathologyCoordinateSpace,
    StructuredReportReferenceKind, ViewerSourceIdentity,
};

use super::{load_report, ReportEvent, ReportState};
use crate::app::camera::CameraView;
use crate::app::tests::{run_ui, wait_for_background, write_source_wsi};
use crate::app::workspace::{draw_external_layer_overlays, WorkspaceRuntime};

const MAPPING: &str = r#"{
  "schema_version":1,
  "labels":{"tumor":{
    "category":{"code_value":"MORPH","coding_scheme_designator":"99WSI","code_meaning":"Morphology"},
    "property_type":{"code_value":"TUMOR","coding_scheme_designator":"99WSI","code_meaning":"Tumor"},
    "generation_type":"MANUAL",
    "recommended_display_cielab":[39321,38036,35466],
    "segment_label":"Tumor"
  }},
  "measurements":{"Area":{
    "concept":{"code_value":"AREA","coding_scheme_designator":"99WSI","code_meaning":"Area"},
    "unit":{"code_value":"mm2","coding_scheme_designator":"UCUM","code_meaning":"square millimeter"}
  }},
  "qualitative_evaluations":{},
  "sr":{
    "report_title":{"code_value":"126000","coding_scheme_designator":"DCM","code_meaning":"Imaging Measurement Report"},
    "procedures_reported":[{"code_value":"P5-09051","coding_scheme_designator":"SRT","code_meaning":"Histopathology procedure"}]
  }
}"#;

const GEOJSON: &str = r#"{
  "type":"FeatureCollection",
  "features":[{
    "type":"Feature",
    "id":"2.25.710",
    "geometry":{"type":"Polygon","coordinates":[[[1,1],[6,1],[6,6],[1,6],[1,1]]]},
    "properties":{"classification":{"name":"tumor"},"measurements":{"Area":25}}
  }]
}"#;

const GEOJSON_WITH_HOLE: &str = r#"{
  "type":"FeatureCollection",
  "features":[{
    "type":"Feature",
    "id":"2.25.711",
    "geometry":{"type":"Polygon","coordinates":[
      [[1,1],[7,1],[7,7],[1,7],[1,1]],
      [[3,3],[3,5],[5,5],[5,3],[3,3]]
    ]},
    "properties":{"classification":{"name":"tumor"}}
  }]
}"#;

#[test]
fn loads_direct_sr_content_and_pixel_overlay_without_a_companion_seg() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    let report_path = directory.path().join("report.dcm");
    write_source_wsi(&source_path);
    let context = DicomAnnotationContext::from_source(&source_path).unwrap();
    let annotations = PathologyAnnotationSet::from_json(
        GEOJSON.as_bytes(),
        MAPPING.as_bytes(),
        &context,
        &context,
        PathologyCoordinateSpace::Level0Pixels,
        false,
    )
    .unwrap();
    annotations
        .to_sr(None)
        .unwrap()
        .write_sr(&report_path)
        .unwrap();

    let repaint = eframe::egui::Context::default();
    let mut state = ReportState::default();
    state
        .start_load(report_path.clone(), None, context.clone(), &repaint)
        .unwrap();
    assert!(state.is_busy());
    assert!(state
        .start_load(report_path.clone(), None, context.clone(), &repaint)
        .unwrap_err()
        .contains("already loading"));
    let session = match wait_for_background("structured report import", || state.poll()) {
        ReportEvent::Loaded {
            path,
            group_count,
            session,
        } => {
            assert_eq!(path, report_path);
            assert_eq!(group_count, 1);
            session
        }
        _ => panic!("structured report should load successfully"),
    };

    assert_eq!(
        session.document().report_title().meaning(),
        "Imaging Measurement Report"
    );
    assert_eq!(session.document().completion_flag(), "COMPLETE");
    assert_eq!(session.document().verification_flag(), "UNVERIFIED");
    assert_eq!(session.document().preliminary_flag(), "PRELIMINARY");
    assert_eq!(
        session.document().groups()[0].measurements()[0].value(),
        25.0
    );
    assert_eq!(
        session.document().groups()[0].reference_kind(),
        StructuredReportReferenceKind::Polygon
    );
    assert_eq!(session.regions().len(), 1);
    assert_eq!(session.regions()[0].points().len(), 5);
    assert_eq!(
        session.regions()[0].graphic(),
        dicom_viewer_core::CoordinateGraphic::Polygon
    );
    for (actual, expected) in session.regions()[0]
        .bounds()
        .into_iter()
        .zip([1.0, 1.0, 6.0, 6.0])
    {
        assert!((actual - expected).abs() < 1e-5);
    }
    assert!(session.mask_runs().is_empty());

    let mut runtime = WorkspaceRuntime::new(
        ViewerSourceIdentity::new(2, 0, 0, 0, 0, 0, (8, 8)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap();
    runtime
        .add_external_report("Imported SR", Some(report_path.clone()), *session)
        .unwrap();
    let output = run_ui(|ui| {
        let viewport = eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(0.0, 0.0),
            eframe::egui::vec2(100.0, 100.0),
        );
        draw_external_layer_overlays(
            &ui.painter_at(viewport),
            viewport,
            &runtime,
            &context,
            CameraView {
                center_base: eframe::egui::vec2(4.0, 4.0),
                zoom: 10.0,
            },
        );
    });
    assert!(!output.shapes.is_empty());

    state.clear();
    let missing = directory.path().join("missing-report.dcm");
    state
        .start_load(missing.clone(), None, context, &repaint)
        .unwrap();
    match wait_for_background("invalid structured report", || state.poll()) {
        ReportEvent::Failed { path, error } => {
            assert_eq!(path, missing);
            assert!(error.contains("Open SR + SEG"));
        }
        _ => panic!("missing structured report should fail"),
    }
}

#[test]
fn loads_seg_referenced_sr_as_an_exact_mask_overlay() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    let report_path = directory.path().join("report.dcm");
    let segmentation_path = directory.path().join("seg.dcm");
    write_source_wsi(&source_path);
    let context = DicomAnnotationContext::from_source(&source_path).unwrap();
    let annotations = PathologyAnnotationSet::from_json(
        GEOJSON_WITH_HOLE.as_bytes(),
        MAPPING.as_bytes(),
        &context,
        &context,
        PathologyCoordinateSpace::Level0Pixels,
        false,
    )
    .unwrap();
    let segmentation = annotations.to_seg(true).unwrap();
    let report = annotations.to_sr(Some(&segmentation)).unwrap();
    segmentation.write_seg(&segmentation_path).unwrap();
    report.write_sr(&report_path).unwrap();

    let session = load_report(report_path, Some(segmentation_path.clone()), &context).unwrap();

    assert!(session.regions().is_empty());
    assert!(!session.mask_runs().is_empty());
    assert_eq!(
        session.document().groups()[0].reference_kind(),
        StructuredReportReferenceKind::Segmentation
    );

    let mut runtime = WorkspaceRuntime::new(
        ViewerSourceIdentity::new(3, 0, 0, 0, 0, 0, (8, 8)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap();
    runtime
        .add_external_report("Imported SR", None, session)
        .unwrap();
    let output = run_ui(|ui| {
        let viewport = eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(0.0, 0.0),
            eframe::egui::vec2(100.0, 100.0),
        );
        draw_external_layer_overlays(
            &ui.painter_at(viewport),
            viewport,
            &runtime,
            &context,
            CameraView {
                center_base: eframe::egui::vec2(4.0, 4.0),
                zoom: 10.0,
            },
        );
    });
    assert!(!output.shapes.is_empty());
}
