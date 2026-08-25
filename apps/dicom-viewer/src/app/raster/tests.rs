use std::fs;
use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use dicom_dictionary_std::tags;
use dicom_viewer_core::{AnnotationScheme, DicomAnnotationContext, ViewerSourceIdentity};

use super::{
    export_raster_cancellable, load_raster, preview_image, RasterEvent, RasterInput, RasterState,
};
use crate::app::camera::CameraView;
use crate::app::export_job::{WorkspaceExportJob, WorkspaceExportKind};
use crate::app::tests::{wait_for_background, write_source_wsi};
use crate::app::workspace::{draw_external_layer_overlays, ExternalLayerPayload, WorkspaceRuntime};

const PROFILE: &str = r#"{
  "schema_version":1,
  "input_format":"npy",
  "dtype":"float32",
  "axes":["y","x"],
  "grid_origin":{"x":0,"y":0},
  "sample_spacing":{"x":1,"y":1},
  "coordinate_space":"source-pixels",
  "channels":[{
    "name":"tumor",
    "quantity":{"code_value":"TUMOR","coding_scheme_designator":"99WSI","code_meaning":"Tumor probability"},
    "unit":{"code_value":"1","coding_scheme_designator":"UCUM","code_meaning":"no units"}
  }],
  "algorithm":{
    "family":{"code_value":"AI","coding_scheme_designator":"99WSI","code_meaning":"AI"},
    "name":"test-model",
    "version":"1"
  }
}"#;

#[test]
fn raster_preview_and_pm_bundle_revalidate_the_same_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    let profile_path = directory.path().join("profile.json");
    let raster_path = directory.path().join("probability.npy");
    write_source_wsi(&source_path);
    fs::write(&profile_path, PROFILE).unwrap();
    write_npy_f32(&raster_path, &[0.0, 0.25, 0.75, 1.0]);
    let source = DicomAnnotationContext::from_source(&source_path).unwrap();
    let input = RasterInput {
        source,
        profile_path,
        raster_path: raster_path.clone(),
    };

    let session = load_raster(input.clone()).unwrap();

    assert_eq!(session.preview().dimensions(), (2, 2));
    assert_eq!(session.selected_channel_count(), 1);
    assert_eq!(session.frame_count(), 1);
    let cancellation = AtomicBool::new(false);

    write_npy_f32(&raster_path, &[0.0, 0.25, 0.5, 1.0]);
    let stale_destination = directory.path().join("stale-pm");
    let error = export_raster_cancellable(&session, &stale_destination, &cancellation).unwrap_err();
    assert!(error.contains("changed after preview"));
    assert!(!stale_destination.exists());

    write_npy_f32(&raster_path, &[0.0, 0.25, 0.75, 1.0]);
    let destination = directory.path().join("pm-bundle");
    let (published, count) =
        export_raster_cancellable(&session, &destination, &cancellation).unwrap();
    assert_eq!(published, destination.canonicalize().unwrap());
    assert_eq!(count, 1);
    assert!(destination.join("pm-0001.dcm").is_file());
    assert!(destination.join("manifest.json").is_file());
    let object = dicom_object::open_file(destination.join("pm-0001.dcm")).unwrap();
    assert_eq!(
        object
            .element(tags::MANUFACTURER)
            .unwrap()
            .to_str()
            .unwrap(),
        "Frames"
    );
    assert_eq!(
        object
            .element(tags::MANUFACTURER_MODEL_NAME)
            .unwrap()
            .to_str()
            .unwrap(),
        "DICOM Viewer"
    );
}

#[test]
fn cancelled_pm_export_never_publishes_the_destination() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    let profile_path = directory.path().join("profile.json");
    let raster_path = directory.path().join("probability.npy");
    write_source_wsi(&source_path);
    fs::write(&profile_path, PROFILE).unwrap();
    write_npy_f32(&raster_path, &[0.0, 0.25, 0.75, 1.0]);
    let source = DicomAnnotationContext::from_source(&source_path).unwrap();
    let input = RasterInput {
        source,
        profile_path,
        raster_path,
    };
    let session = load_raster(input).unwrap();
    let cancellation = AtomicBool::new(true);
    let destination = directory.path().join("cancelled-pm");

    let error = export_raster_cancellable(&session, &destination, &cancellation).unwrap_err();
    assert!(error.contains("cancelled"));
    assert!(!destination.exists());
}

#[test]
fn heatmap_texture_keeps_missing_samples_transparent_and_paints_registered_mesh() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    let profile_path = directory.path().join("profile.json");
    let raster_path = directory.path().join("probability.npy");
    write_source_wsi(&source_path);
    fs::write(&profile_path, PROFILE).unwrap();
    write_npy_f32(&raster_path, &[0.0, f32::NAN, 0.75, 1.0]);
    let source = DicomAnnotationContext::from_source(&source_path).unwrap();
    let session = load_raster(RasterInput {
        source: source.clone(),
        profile_path,
        raster_path,
    })
    .unwrap();
    let image = preview_image(session.preview(), 0.0, 1.0);
    assert_eq!(image.pixels[1], eframe::egui::Color32::TRANSPARENT);
    assert_ne!(image.pixels[0], image.pixels[3]);

    let context = eframe::egui::Context::default();
    let mut runtime = WorkspaceRuntime::new(
        ViewerSourceIdentity::new(4, 0, 0, 0, 0, 0, (2, 2)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap();
    let layer_id = runtime
        .add_external_heatmap("Tumor probability", session, &context)
        .unwrap();
    assert!(matches!(
        runtime.external_payload(layer_id),
        Some(ExternalLayerPayload::Heatmap { .. })
    ));
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
                center_base: eframe::egui::vec2(1.0, 1.0),
                zoom: 20.0,
            },
        );
    });
    assert!(!output.shapes.is_empty());
    assert!(!output.textures_delta.set.is_empty());

    runtime.set_layer_visibility(layer_id, false).unwrap();
    let hidden = context.run_ui(eframe::egui::RawInput::default(), |ui| {
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
                center_base: eframe::egui::vec2(1.0, 1.0),
                zoom: 20.0,
            },
        );
    });
    assert!(hidden.shapes.is_empty());
}

#[test]
fn raster_state_transfers_heatmap_and_common_export_job_publishes_pm() {
    let mut state = RasterState::default();
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.dcm");
    let profile_path = directory.path().join("profile.json");
    let raster_path = directory.path().join("probability.npy");
    write_source_wsi(&source_path);
    fs::write(&profile_path, PROFILE).unwrap();
    write_npy_f32(&raster_path, &[0.0, 0.25, 0.75, 1.0]);
    let source = DicomAnnotationContext::from_source(&source_path).unwrap();
    let repaint = eframe::egui::Context::default();

    state
        .start_load(
            source.clone(),
            profile_path.clone(),
            raster_path.clone(),
            &repaint,
        )
        .unwrap();
    assert!(state.is_busy());
    assert!(state
        .start_load(
            source.clone(),
            profile_path.clone(),
            raster_path.clone(),
            &repaint,
        )
        .unwrap_err()
        .contains("already running"));
    let session = match wait_for_background("raster import", || state.poll()) {
        RasterEvent::Loaded(session) => session,
        _ => panic!("raster import should complete successfully"),
    };
    assert_eq!(session.selected_channel_count(), 1);
    assert_eq!(session.frame_count(), 1);
    assert_eq!(session.profile_path(), profile_path);
    assert_eq!(session.raster_path(), raster_path);

    let source_identity = ViewerSourceIdentity::new(4, 0, 0, 0, 0, 0, (2, 2));
    let mut runtime =
        WorkspaceRuntime::new(source_identity, AnnotationScheme::general_pathology_v1()).unwrap();
    let layer_id = runtime
        .add_external_heatmap("Tumor probability", *session, &repaint)
        .unwrap();
    let export_session = match runtime.external_payload(layer_id) {
        Some(ExternalLayerPayload::Heatmap { session, .. }) => Arc::clone(session),
        _ => panic!("loaded raster should be owned by the workspace layer"),
    };

    let destination = directory.path().join("background-pm");
    let job = WorkspaceExportJob::spawn_published(
        destination.clone(),
        WorkspaceExportKind::DicomPm,
        &repaint,
        move |destination, cancellation| {
            export_raster_cancellable(&export_session, destination, cancellation).map(|_| ())
        },
    )
    .unwrap();
    let result = wait_for_background("Parametric Map export", || match job.poll() {
        crate::app::background_worker::WorkerPoll::Pending => None,
        crate::app::background_worker::WorkerPoll::Complete(result) => Some(result),
        crate::app::background_worker::WorkerPoll::Disconnected => {
            panic!("Parametric Map export worker disconnected")
        }
    });
    assert!(result.result.is_ok());
    assert_eq!(result.kind, WorkspaceExportKind::DicomPm);
    assert_eq!(result.destination, destination);
    assert!(destination.join("pm-0001.dcm").is_file());

    state.clear();
    assert!(!state.is_busy());

    let source = DicomAnnotationContext::from_source(&source_path).unwrap();
    state
        .start_load(
            source,
            directory.path().join("missing-profile.json"),
            raster_path,
            &repaint,
        )
        .unwrap();
    match wait_for_background("missing raster profile", || state.poll()) {
        RasterEvent::Failed(error) => {
            assert!(error.contains("could not inspect raster profile"));
        }
        _ => panic!("missing raster profile should fail"),
    }
}

fn write_npy_f32(path: &std::path::Path, values: &[f32]) {
    let mut file = fs::File::create(path).unwrap();
    file.write_all(b"\x93NUMPY\x01\x00").unwrap();
    let mut header = "{'descr': '<f4', 'fortran_order': False, 'shape': (2, 2), }"
        .as_bytes()
        .to_vec();
    let padding = (16 - ((10 + header.len() + 1) % 16)) % 16;
    header.extend(std::iter::repeat_n(b' ', padding));
    header.push(b'\n');
    file.write_all(&(header.len() as u16).to_le_bytes())
        .unwrap();
    file.write_all(&header).unwrap();
    for value in values {
        file.write_all(&value.to_le_bytes()).unwrap();
    }
}
