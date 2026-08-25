use std::collections::HashMap;

use dicom_viewer_core::{Point2, VectorFindingGeometry};
use eframe::egui::{self, Color32, Mesh, Rect, Shape, Stroke, Vec2};

use super::spatial::SpatialRenderGeometry;
use super::WorkspaceRuntime;
use crate::app::camera::CameraView;
use crate::app::theme;
use crate::app::viewport::visible_base_bounds;

// Characterized against the release overlay workload documented in
// docs/PATHOLOGY_PERFORMANCE.md. The bound applies to detailed non-point
// objects. Every visible point is retained and screen-bin aggregated, while
// selected objects are always submitted in full detail.
const DETAILED_OBJECT_BUDGET: usize = 8_000;
const DETAILED_VERTEX_BUDGET: usize = 160_000;
const SCREEN_BIN_PIXELS: f32 = 4.0;
const DISPLAY_DECIMATION_PIXELS: f32 = 0.5;

pub(in crate::app) fn draw_workspace_overlay(
    painter: &egui::Painter,
    viewport: Rect,
    runtime: &WorkspaceRuntime,
    view: CameraView,
) {
    draw_workspace_overlay_with_budgets(
        painter,
        viewport,
        runtime,
        view,
        DETAILED_OBJECT_BUDGET,
        DETAILED_VERTEX_BUDGET,
    );
}

fn draw_workspace_overlay_with_budgets(
    painter: &egui::Painter,
    viewport: Rect,
    runtime: &WorkspaceRuntime,
    view: CameraView,
    detailed_object_budget: usize,
    detailed_vertex_budget: usize,
) {
    let visible = visible_base_bounds(viewport, view.center_base, view.zoom);
    let query = runtime.spatial_index().query_for_overlay(
        visible,
        detailed_object_budget,
        runtime.selection(),
    );
    let document = runtime.document();
    let mut shapes = Vec::new();
    let mut point_bins =
        HashMap::<(i32, i32, [u8; 4]), (egui::Pos2, Color32, usize)>::with_capacity(
            query.points().len(),
        );
    let mut detailed_vertices = 0usize;

    for point in query.points() {
        if !document.presentation().object_visible(point.id()) {
            continue;
        }
        let layer_presentation = document.presentation().layer(point.layer_id());
        if !layer_presentation.visible {
            continue;
        }
        let color =
            class_color(runtime, point.class_id()).gamma_multiply(layer_presentation.opacity);
        let screen = screen_point(viewport, point.point(), view);
        let key = (
            (screen.x / SCREEN_BIN_PIXELS).floor() as i32,
            (screen.y / SCREEN_BIN_PIXELS).floor() as i32,
            color.to_array(),
        );
        point_bins
            .entry(key)
            .and_modify(|bin| bin.2 += 1)
            .or_insert((screen, color, 1));
    }

    for &id in query.detailed_ids() {
        if !document.presentation().object_visible(id) {
            continue;
        }
        let selected = runtime.selection().contains(&id);
        let Some(object) = runtime.spatial_index().render_object(id) else {
            continue;
        };
        if !selected {
            let Some(next_count) = detailed_vertices.checked_add(object.vertex_count()) else {
                continue;
            };
            if next_count > detailed_vertex_budget {
                continue;
            }
            detailed_vertices = next_count;
        }
        let opacity = if let Some(layer_id) = object.layer_id() {
            let layer_presentation = document.presentation().layer(layer_id);
            if !layer_presentation.visible {
                continue;
            }
            layer_presentation.opacity
        } else {
            1.0
        };
        let color = class_color(runtime, object.class_id()).gamma_multiply(opacity);
        match object.geometry() {
            SpatialRenderGeometry::Vector(geometry) => match geometry {
                VectorFindingGeometry::Point(point) => {
                    let screen = screen_point(viewport, *point, view);
                    if selected {
                        draw_point(&mut shapes, screen, color, true);
                    } else {
                        let key = (
                            (screen.x / SCREEN_BIN_PIXELS).floor() as i32,
                            (screen.y / SCREEN_BIN_PIXELS).floor() as i32,
                            color.to_array(),
                        );
                        point_bins
                            .entry(key)
                            .and_modify(|bin| bin.2 += 1)
                            .or_insert((screen, color, 1));
                    }
                }
                VectorFindingGeometry::Regions(components) => {
                    for component in components {
                        draw_polygon(
                            &mut shapes,
                            viewport,
                            component,
                            view,
                            color,
                            selected,
                            false,
                        );
                    }
                }
            },
            SpatialRenderGeometry::Segment {
                composite,
                primitives,
            } => {
                for component in composite.components() {
                    draw_polygon(
                        &mut shapes,
                        viewport,
                        component.exterior(),
                        view,
                        color,
                        selected,
                        true,
                    );
                    for hole in component.holes() {
                        shapes.push(Shape::closed_line(
                            screen_points_for_render(viewport, hole, view, selected),
                            Stroke::new(1.5, color.gamma_multiply(0.8)),
                        ));
                    }
                }
                if selected {
                    for primitive in primitives.iter() {
                        let points = match primitive.geometry() {
                            dicom_viewer_core::SegmentationPrimitiveGeometry::Polygon {
                                points,
                            } => points.as_ref(),
                            dicom_viewer_core::SegmentationPrimitiveGeometry::Brush {
                                centerline,
                                ..
                            } => centerline.as_ref(),
                        };
                        for point in points {
                            draw_handle(
                                &mut shapes,
                                screen_point(viewport, *point, view),
                                color,
                                true,
                            );
                        }
                    }
                }
            }
            SpatialRenderGeometry::Measurement(endpoints) => {
                let points = endpoints.map(|point| screen_point(viewport, point, view));
                if selected {
                    shapes.push(Shape::line_segment(
                        points,
                        Stroke::new(4.0, Color32::WHITE.gamma_multiply(0.75)),
                    ));
                }
                shapes.push(Shape::line_segment(points, Stroke::new(2.0, color)));
                for point in points {
                    draw_handle(&mut shapes, point, color, selected);
                }
            }
        }
    }

    let mut point_bins = point_bins.into_iter().collect::<Vec<_>>();
    point_bins.sort_unstable_by_key(|(key, _)| *key);
    let mut point_mesh = Mesh::default();
    point_mesh.reserve_vertices(point_bins.len().saturating_mul(4));
    point_mesh.reserve_triangles(point_bins.len().saturating_mul(2));
    for (_, (screen, color, count)) in point_bins {
        let radius = if count > 1 { 3.5 } else { 2.5 };
        add_point_marker(&mut point_mesh, screen, radius, color);
    }
    if !point_mesh.is_empty() {
        shapes.push(Shape::mesh(point_mesh));
    }

    if let Some(draft) = runtime.draft() {
        let points = screen_points(viewport, draft.points(), view);
        if points.len() >= 2 {
            shapes.push(Shape::line(
                points.clone(),
                Stroke::new(2.0, theme::AMBER_BRIGHT),
            ));
        }
        for point in points {
            draw_handle(&mut shapes, point, theme::AMBER_BRIGHT, true);
        }
    }
    if let Some(stroke) = runtime.brush_stroke() {
        let points = screen_points(viewport, stroke, view);
        if points.len() >= 2 {
            shapes.push(Shape::line(
                points,
                Stroke::new(
                    (runtime.brush_diameter() as f32 * view.zoom).max(1.0),
                    theme::AMBER_BRIGHT.gamma_multiply(0.35),
                ),
            ));
        }
    }
    if let Some(start) = runtime.ruler_start() {
        draw_handle(
            &mut shapes,
            screen_point(viewport, start, view),
            theme::AMBER_BRIGHT,
            true,
        );
    }
    painter.extend(shapes);
}

fn draw_polygon(
    shapes: &mut Vec<Shape>,
    viewport: Rect,
    points: &[Point2],
    view: CameraView,
    color: Color32,
    selected: bool,
    filled: bool,
) {
    let screen = screen_points_for_render(viewport, points, view, selected);
    if screen.len() < 3 {
        return;
    }
    shapes.push(Shape::closed_line(
        screen.clone(),
        Stroke::new(
            if selected {
                2.5
            } else if filled {
                1.8
            } else {
                1.4
            },
            color,
        ),
    ));
    if selected {
        shapes.push(Shape::closed_line(
            screen.clone(),
            Stroke::new(3.8, Color32::WHITE.gamma_multiply(0.65)),
        ));
        shapes.push(Shape::closed_line(screen.clone(), Stroke::new(2.0, color)));
        if !filled {
            for point in screen {
                draw_handle(shapes, point, color, true);
            }
        }
    }
}

fn add_point_marker(mesh: &mut Mesh, center: egui::Pos2, radius: f32, color: Color32) {
    let first = mesh.vertices.len() as u32;
    mesh.colored_vertex(center + Vec2::new(0.0, -radius), color);
    mesh.colored_vertex(center + Vec2::new(radius, 0.0), color);
    mesh.colored_vertex(center + Vec2::new(0.0, radius), color);
    mesh.colored_vertex(center + Vec2::new(-radius, 0.0), color);
    mesh.add_triangle(first, first + 1, first + 2);
    mesh.add_triangle(first, first + 2, first + 3);
}

fn draw_point(shapes: &mut Vec<Shape>, point: egui::Pos2, color: Color32, selected: bool) {
    if selected {
        shapes.push(Shape::circle_filled(point, 6.0, Color32::WHITE));
    }
    shapes.push(Shape::circle_filled(
        point,
        if selected { 4.0 } else { 2.5 },
        color,
    ));
}

fn draw_handle(shapes: &mut Vec<Shape>, point: egui::Pos2, color: Color32, selected: bool) {
    shapes.push(Shape::circle_filled(
        point,
        if selected { 5.0 } else { 3.5 },
        theme::CANVAS_EDGE,
    ));
    shapes.push(Shape::circle_filled(
        point,
        if selected { 3.2 } else { 2.2 },
        color,
    ));
}

fn class_color(runtime: &WorkspaceRuntime, class_id: &str) -> Color32 {
    let rgb = runtime
        .document()
        .scheme()
        .class(class_id)
        .map(|class| class.display_color())
        .unwrap_or([160, 160, 160]);
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

fn screen_points(viewport: Rect, points: &[Point2], view: CameraView) -> Vec<egui::Pos2> {
    points
        .iter()
        .map(|point| screen_point(viewport, *point, view))
        .collect()
}

fn screen_points_for_render(
    viewport: Rect,
    points: &[Point2],
    view: CameraView,
    selected: bool,
) -> Vec<egui::Pos2> {
    let screen = screen_points(viewport, points, view);
    if selected || screen.len() <= 3 {
        return screen;
    }

    let minimum_distance_squared = DISPLAY_DECIMATION_PIXELS * DISPLAY_DECIMATION_PIXELS;
    let mut display = Vec::with_capacity(screen.len());
    display.push(screen[0]);
    for point in screen.iter().copied().skip(1) {
        if point.distance_sq(*display.last().expect("display begins with one point"))
            >= minimum_distance_squared
        {
            display.push(point);
        }
    }
    if display.len() >= 3 {
        display
    } else {
        vec![
            screen[0],
            screen[screen.len() / 3],
            screen[2 * screen.len() / 3],
        ]
    }
}

fn screen_point(viewport: Rect, point: Point2, view: CameraView) -> egui::Pos2 {
    view.base_to_screen(viewport, Vec2::new(point.x as f32, point.y as f32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::run_ui;
    use dicom_viewer_core::{AnnotationScheme, ViewerSourceIdentity, WorkspaceDocument};
    use std::time::Instant;

    #[test]
    fn overlay_uses_viewport_index_and_emits_selected_handles() {
        let mut runtime = WorkspaceRuntime::new(
            ViewerSourceIdentity::new(4, 0, 0, 0, 0, 0, (500, 500)),
            AnnotationScheme::general_pathology_v1(),
        )
        .unwrap();
        let layer = runtime.active_vector_layer();
        let finding = runtime
            .edit("add", |document| {
                document.add_vector_finding(
                    layer,
                    "neoplasm",
                    VectorFindingGeometry::regions(vec![vec![
                        Point2::new(10.0, 10.0),
                        Point2::new(30.0, 10.0),
                        Point2::new(30.0, 30.0),
                        Point2::new(10.0, 30.0),
                    ]]),
                )
            })
            .unwrap();
        runtime.select_only(finding);
        runtime.refresh_spatial_index().unwrap();
        let output = run_ui(|ui| {
            let viewport = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0));
            draw_workspace_overlay(
                &ui.painter_at(viewport),
                viewport,
                &runtime,
                CameraView {
                    center_base: Vec2::new(50.0, 50.0),
                    zoom: 1.0,
                },
            );
        });
        assert!(!output.shapes.is_empty());
    }

    #[test]
    fn display_decimation_is_subpixel_only_and_never_changes_stored_coordinates() {
        let points = (0..100)
            .map(|index| Point2::new(index as f64 * 0.1, (index % 3) as f64 * 0.1))
            .collect::<Vec<_>>();
        let original = points.clone();
        let viewport = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0));
        let view = CameraView {
            center_base: Vec2::ZERO,
            zoom: 1.0,
        };

        let decimated = screen_points_for_render(viewport, &points, view, false);
        let selected = screen_points_for_render(viewport, &points, view, true);

        assert!(decimated.len() >= 3);
        assert!(decimated.len() < points.len());
        assert_eq!(selected.len(), points.len());
        assert_eq!(points, original);
    }

    #[test]
    fn dense_point_bins_are_one_valid_mesh() {
        let mut mesh = Mesh::default();
        add_point_marker(&mut mesh, egui::pos2(10.0, 12.0), 3.5, Color32::RED);
        add_point_marker(&mut mesh, egui::pos2(20.0, 22.0), 2.5, Color32::BLUE);

        assert!(mesh.is_valid());
        assert_eq!(mesh.vertices.len(), 8);
        assert_eq!(mesh.indices.len(), 12);
    }

    #[test]
    #[ignore = "manual optimized-build characterization; see docs/PATHOLOGY_PERFORMANCE.md"]
    fn pathology_overlay_release_characterization() {
        if cfg!(debug_assertions) {
            panic!("run this characterization with cargo test --release");
        }
        const POINT_COUNT: usize = 50_000;
        const REGION_COUNT: usize = 20_000;
        const REGION_VERTICES: usize = 20;
        const SAMPLES: usize = 15;

        let source = ViewerSourceIdentity::new(44, 0, 0, 0, 0, 0, (16_384, 16_384));
        let mut document =
            WorkspaceDocument::new(source, AnnotationScheme::general_pathology_v1()).unwrap();
        let layer = document.vector_layers()[0].id();
        for index in 0..POINT_COUNT {
            let x = (index % 400) as f64 * 40.0 + 10.0;
            let y = (index / 400) as f64 * 125.0 + 10.0;
            document
                .add_vector_finding(
                    layer,
                    "cell",
                    VectorFindingGeometry::Point(Point2::new(x, y)),
                )
                .unwrap();
        }
        for index in 0..REGION_COUNT {
            let center_x = (index % 200) as f64 * 80.0 + 40.0;
            let center_y = (index / 200) as f64 * 160.0 + 80.0;
            let points = (0..REGION_VERTICES)
                .map(|vertex| {
                    let angle = std::f64::consts::TAU * vertex as f64 / REGION_VERTICES as f64;
                    Point2::new(center_x + angle.cos() * 20.0, center_y + angle.sin() * 20.0)
                })
                .collect::<Vec<_>>();
            document
                .add_vector_finding(
                    layer,
                    "neoplasm",
                    VectorFindingGeometry::regions(vec![points]),
                )
                .unwrap();
        }
        let runtime = WorkspaceRuntime::from_document(document).unwrap();
        let viewport = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0));
        let view = CameraView {
            center_base: Vec2::splat(8_192.0),
            zoom: 1080.0 / 16_384.0,
        };

        for (object_budget, vertex_budget) in [
            (4_000, 80_000),
            (8_000, 160_000),
            (12_000, 240_000),
            (16_000, 320_000),
        ] {
            let mut milliseconds = Vec::with_capacity(SAMPLES);
            let mut shape_count = 0usize;
            for _ in 0..SAMPLES {
                let started = Instant::now();
                let output = run_ui(|ui| {
                    draw_workspace_overlay_with_budgets(
                        &ui.painter_at(viewport),
                        viewport,
                        &runtime,
                        view,
                        object_budget,
                        vertex_budget,
                    );
                });
                milliseconds.push(started.elapsed().as_secs_f64() * 1_000.0);
                shape_count = std::hint::black_box(output.shapes.len());
            }
            milliseconds.sort_by(f64::total_cmp);
            let percentile = |fraction: f64| {
                let index = ((milliseconds.len() - 1) as f64 * fraction).ceil() as usize;
                milliseconds[index]
            };
            eprintln!(
                "pathology-overlay objects={object_budget} vertices={vertex_budget} \
                 points={POINT_COUNT} regions={REGION_COUNT} region_vertices={REGION_VERTICES} \
                 shapes={shape_count} p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
                percentile(0.50),
                percentile(0.95),
                milliseconds[milliseconds.len() - 1]
            );
        }
    }
}
