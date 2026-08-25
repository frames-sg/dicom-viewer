use std::fs;

use dicom_viewer_core::{AnnotationScheme, DicomAnnotationContext, ViewerSourceIdentity};

use super::io::load_pathology;
use super::{PathologyEvent, PathologyInput, PathologyState};
use crate::app::camera::CameraView;
use crate::app::tests::{wait_for_background, write_source_wsi};
use crate::app::workspace::{draw_external_layer_overlays, ExternalLayerPayload, WorkspaceRuntime};

const MAPPING: &str = r#"{
  "schema_version":1,
  "labels":{"tumor":{
    "category":{"code_value":"MORPH","coding_scheme_designator":"99WSI","code_meaning":"Morphology"},
    "property_type":{"code_value":"TUMOR","coding_scheme_designator":"99WSI","code_meaning":"Tumor"},
    "generation_type":"MANUAL",
    "recommended_display_cielab":[39321,38036,35466],
    "segment_label":"Tumor"
  }},
  "measurements":{},
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
    "id":"2.25.901",
    "geometry":{"type":"Polygon","coordinates":[[[1,1],[7,1],[7,7],[1,7],[1,1]]]},
    "properties":{"classification":{"name":"tumor"}}
  }]
}"#;

#[test]
fn profiled_geojson_import_builds_a_lossless_editable_preview() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    let geojson_path = directory.path().join("annotations.geojson");
    let mapping_path = directory.path().join("mapping.json");
    write_source_wsi(&source_path);
    fs::write(&geojson_path, GEOJSON).unwrap();
    fs::write(&mapping_path, MAPPING).unwrap();
    let source = DicomAnnotationContext::from_source(&source_path).unwrap();
    let session = load_pathology(PathologyInput {
        source,
        geojson_path,
        mapping_path,
    })
    .unwrap();

    assert_eq!(session.preview.features().len(), 1);
    assert!(session.editable_ann.is_some());
    assert!(session.editable_ann().is_some());
}

#[test]
fn pathology_state_runs_import_preview_and_overlay_as_background_work() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    let geojson_path = directory.path().join("annotations.geojson");
    let mapping_path = directory.path().join("mapping.json");
    write_source_wsi(&source_path);
    fs::write(&geojson_path, GEOJSON).unwrap();
    fs::write(&mapping_path, MAPPING).unwrap();
    let source = DicomAnnotationContext::from_source(&source_path).unwrap();
    let repaint = eframe::egui::Context::default();
    let mut state = PathologyState::default();

    state
        .start_load(
            source.clone(),
            geojson_path.clone(),
            mapping_path.clone(),
            &repaint,
        )
        .unwrap();
    assert!(state.is_busy());
    assert!(state
        .start_load(
            source.clone(),
            geojson_path.clone(),
            mapping_path.clone(),
            &repaint,
        )
        .unwrap_err()
        .contains("already running"));
    let session = match wait_for_background("pathology import", || state.poll()) {
        PathologyEvent::Loaded(session) => session,
        _ => panic!("pathology import should complete successfully"),
    };
    assert!(!state.is_busy());
    assert_eq!(session.geojson_path(), geojson_path);
    assert_eq!(session.mapping_path(), mapping_path);
    assert!(session.editable_ann().is_some());
    assert!(session.diagnostic_count() > 0);

    let mut runtime = WorkspaceRuntime::new(
        ViewerSourceIdentity::new(1, 0, 0, 0, 0, 0, (8, 8)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap();
    let layer_id = runtime
        .add_external_pathology("Profiled GeoJSON", *session)
        .unwrap();
    assert!(matches!(
        runtime.external_payload(layer_id),
        Some(ExternalLayerPayload::ProfiledGeoJson(_))
    ));

    let context = eframe::egui::Context::default();
    let output = context.run_ui(eframe::egui::RawInput::default(), |ui| {
        let viewport = eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(0.0, 0.0),
            eframe::egui::vec2(100.0, 100.0),
        );
        draw_external_layer_overlays(
            &ui.painter_at(viewport),
            viewport,
            &runtime,
            &source,
            CameraView {
                center_base: eframe::egui::vec2(4.0, 4.0),
                zoom: 10.0,
            },
        );
    });
    assert!(!output.shapes.is_empty());
    state.clear();

    let source = DicomAnnotationContext::from_source(&source_path).unwrap();
    let missing_geojson = directory.path().join("missing.geojson");
    state
        .start_load(source, missing_geojson, mapping_path, &repaint)
        .unwrap();
    match wait_for_background("missing pathology input", || state.poll()) {
        PathologyEvent::Failed(error) => {
            assert!(error.contains("could not inspect GeoJSON input"));
        }
        _ => panic!("missing pathology input should fail"),
    }
}
