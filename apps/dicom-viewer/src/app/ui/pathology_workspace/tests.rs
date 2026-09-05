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
