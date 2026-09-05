use dicom_viewer_core::{AnnotationScheme, Point2, ViewerSourceIdentity};
use eframe::egui;

use super::{keyboard, pointer};
use crate::app::workspace::{ActiveTool, WorkspaceRuntime};

fn workspace() -> WorkspaceRuntime {
    WorkspaceRuntime::new(
        ViewerSourceIdentity::new(9, 0, 0, 0, 0, 0, (5_000, 4_000)),
        AnnotationScheme::general_pathology_v1(),
    )
    .unwrap()
}

fn press(runtime: &mut WorkspaceRuntime, keys: &[egui::Key], modifiers: egui::Modifiers) {
    let context = egui::Context::default();
    let input = egui::RawInput {
        modifiers,
        events: keys
            .iter()
            .map(|&key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            })
            .collect(),
        ..Default::default()
    };
    let _ = context.run_ui(input, |ui| {
        ui.input(|input| keyboard::handle_keyboard(runtime, &mut String::new(), input));
    });
}

#[test]
fn keyboard_history_precedes_tool_selection_and_restores_cancelled_drafts() {
    let mut runtime = workspace();
    runtime.request_tool(ActiveTool::Polygon);
    runtime.add_polygon_point(Point2::new(10.0, 10.0)).unwrap();
    runtime.add_polygon_point(Point2::new(20.0, 10.0)).unwrap();
    press(&mut runtime, &[egui::Key::Escape], egui::Modifiers::NONE);
    assert_eq!(runtime.draft().unwrap().points().len(), 1);
    press(
        &mut runtime,
        &[egui::Key::Z, egui::Key::V],
        egui::Modifiers {
            command: true,
            ..Default::default()
        },
    );
    assert_eq!(runtime.draft().unwrap().points().len(), 2);
    assert_eq!(runtime.active_tool(), ActiveTool::Polygon);
    let layer_count = runtime.document().segmentation_layers().len();
    press(&mut runtime, &[egui::Key::B], egui::Modifiers::NONE);
    assert_eq!(runtime.active_tool(), ActiveTool::Polygon);
    assert_eq!(runtime.document().segmentation_layers().len(), layer_count);
}

#[test]
fn pointer_capture_loss_cancels_stroke_and_release_commits_one_undoable_edit() {
    let mut runtime = workspace();
    runtime.ensure_segmentation_layer().unwrap();
    runtime.request_tool(ActiveTool::Brush);
    let mut status = String::new();
    let start = pointer::PointerInput {
        point: Some(Point2::new(50.0, 50.0)),
        drag_started: true,
        dragged: true,
        zoom: 1.0,
        ..Default::default()
    };
    assert!(pointer::handle_pointer(&mut runtime, &mut status, &start).drag_consumed);
    assert!(runtime.brush_stroke().is_some());
    pointer::handle_pointer(
        &mut runtime,
        &mut status,
        &pointer::PointerInput {
            capture_lost: true,
            ..Default::default()
        },
    );
    assert!(runtime.brush_stroke().is_none());
    assert_eq!(runtime.document().object_count(), 0);
    assert!(status.contains("cancelled"));

    pointer::handle_pointer(&mut runtime, &mut status, &start);
    let release = pointer::PointerInput {
        drag_stopped: true,
        capture_lost: true,
        ..Default::default()
    };
    assert!(pointer::handle_pointer(&mut runtime, &mut status, &release).drag_consumed);
    assert_eq!(runtime.document().object_count(), 1);
    assert!(runtime.brush_stroke().is_none());
    assert!(runtime.undo());
    assert_eq!(runtime.document().object_count(), 0);
    assert!(runtime.redo());
    assert_eq!(runtime.document().object_count(), 1);
}
