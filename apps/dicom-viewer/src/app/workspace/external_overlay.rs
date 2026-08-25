use dicom_viewer_core::{
    dicom_cielab_to_srgb, AnnotationGeometry, AnnotationGraphicType, AnnotationGroup,
    CoordinateGraphic, DicomAnnotationContext, PathologyPreview, PathologyPreviewGeometry,
    SegmentationDocument,
};
use eframe::egui::epaint::Vertex;
use eframe::egui::{self, pos2, Color32, Mesh, Pos2, Rect, Shape, Stroke, Vec2};

use super::{ExternalLayerPayload, WorkspaceRuntime};
use crate::app::camera::CameraView;
use crate::app::report::{ReportMaskRun, ReportMaskValues, ReportSession};
use crate::app::theme;
use crate::app::viewport::{bounds_intersect, visible_base_bounds};

pub(in crate::app) fn draw_external_layer_overlays(
    painter: &egui::Painter,
    viewport: Rect,
    runtime: &WorkspaceRuntime,
    canonical_source: &DicomAnnotationContext,
    view: CameraView,
) {
    for layer in runtime.document().external_layers() {
        let presentation = runtime.document().presentation().layer(layer.id());
        if !presentation.visible {
            continue;
        }
        match runtime.external_payload(layer.id()) {
            Some(ExternalLayerPayload::Annotation(document)) => draw_groups(
                painter,
                viewport,
                document.groups(),
                Some((document, canonical_source)),
                presentation.opacity,
                view,
            ),
            Some(ExternalLayerPayload::ProfiledGeoJson(session)) => draw_pathology_preview(
                painter,
                viewport,
                session.preview(),
                presentation.opacity,
                view,
            ),
            Some(ExternalLayerPayload::Segmentation {
                document,
                vector_groups,
            }) => {
                if let Some(groups) = vector_groups {
                    draw_groups(painter, viewport, groups, None, presentation.opacity, view);
                } else {
                    draw_fractional_seg(painter, viewport, document, presentation.opacity, view);
                }
            }
            Some(ExternalLayerPayload::Report(session)) => {
                draw_report(painter, viewport, session, presentation.opacity, view)
            }
            Some(ExternalLayerPayload::Heatmap { session, texture }) => draw_heatmap(
                painter,
                viewport,
                session.preview().base_corners(),
                texture,
                presentation.opacity,
                view,
            ),
            None => {}
        }
    }
}

fn draw_heatmap(
    painter: &egui::Painter,
    viewport: Rect,
    base_corners: &[dicom_viewer_core::Point2; 4],
    texture: &egui::TextureHandle,
    opacity: f32,
    view: CameraView,
) {
    let corners = (*base_corners)
        .map(|point| view.base_to_screen(viewport, Vec2::new(point.x as f32, point.y as f32)));
    if !corners.iter().any(|point| viewport.contains(*point))
        && !Rect::from_points(&corners).intersects(viewport)
    {
        return;
    }
    let color = Color32::from_white_alpha((opacity.clamp(0.0, 1.0) * 255.0).round() as u8);
    let mut mesh = Mesh::with_texture(texture.id());
    mesh.vertices = vec![
        Vertex {
            pos: corners[0],
            uv: pos2(0.0, 0.0),
            color,
        },
        Vertex {
            pos: corners[1],
            uv: pos2(1.0, 0.0),
            color,
        },
        Vertex {
            pos: corners[2],
            uv: pos2(1.0, 1.0),
            color,
        },
        Vertex {
            pos: corners[3],
            uv: pos2(0.0, 1.0),
            color,
        },
    ];
    mesh.indices = vec![0, 1, 2, 0, 2, 3];
    painter.add(Shape::mesh(mesh));
}

fn draw_pathology_preview(
    painter: &egui::Painter,
    viewport: Rect,
    preview: &PathologyPreview,
    opacity: f32,
    view: CameraView,
) {
    let visible = visible_base_bounds(viewport, view.center_base, view.zoom);
    for feature in preview.features() {
        if !bounds_intersect(feature.bounds(), visible) {
            continue;
        }
        let rgb = dicom_cielab_to_srgb(feature.recommended_display_cielab());
        let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]).gamma_multiply(opacity);
        let stroke = Stroke::new(2.0, color);
        match feature.geometry() {
            PathologyPreviewGeometry::Points(points) => {
                for point in points {
                    painter.circle_filled(
                        view.base_to_screen(viewport, Vec2::new(point.x as f32, point.y as f32)),
                        3.5,
                        color,
                    );
                }
            }
            PathologyPreviewGeometry::Lines(lines) => {
                for line in lines {
                    painter.add(Shape::line(screen_points(viewport, line, view), stroke));
                }
            }
            PathologyPreviewGeometry::Polygons(polygons) => {
                for polygon in polygons {
                    painter.add(Shape::closed_line(
                        screen_points(viewport, polygon.exterior(), view),
                        stroke,
                    ));
                    for hole in polygon.holes() {
                        painter.add(Shape::closed_line(
                            screen_points(viewport, hole, view),
                            Stroke::new(2.0, color.gamma_multiply(0.75)),
                        ));
                    }
                }
            }
        }
    }
}

fn draw_report(
    painter: &egui::Painter,
    viewport: Rect,
    session: &ReportSession,
    opacity: f32,
    view: CameraView,
) {
    let visible = visible_base_bounds(viewport, view.center_base, view.zoom);
    for run in session.mask_runs() {
        if f64::from(run.row) < visible[1] || f64::from(run.row) > visible[3] {
            continue;
        }
        draw_report_mask_run(painter, viewport, run, opacity, view, visible);
    }
    let color = theme::AMBER_BRIGHT.gamma_multiply(opacity);
    let stroke = Stroke::new(2.25, color);
    for region in session.regions() {
        if !bounds_intersect(region.bounds(), visible) {
            continue;
        }
        let points = region
            .points()
            .iter()
            .map(|point| view.base_to_screen(viewport, Vec2::new(point.x as f32, point.y as f32)))
            .collect::<Vec<_>>();
        match region.graphic() {
            CoordinateGraphic::Point | CoordinateGraphic::Multipoint => {
                for point in points {
                    painter.circle_filled(point, 4.0, color);
                }
            }
            CoordinateGraphic::Polyline => {
                painter.add(Shape::line(points, stroke));
            }
            CoordinateGraphic::Polygon => {
                painter.add(Shape::closed_line(points, stroke));
            }
        };
    }
}

fn draw_report_mask_run(
    painter: &egui::Painter,
    viewport: Rect,
    run: &ReportMaskRun,
    opacity: f32,
    view: CameraView,
    visible: [f64; 4],
) {
    let rgb = dicom_cielab_to_srgb(run.color);
    let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    match &run.values {
        ReportMaskValues::Binary(length) => {
            let end = run.column_start.saturating_add(*length);
            if f64::from(end) < visible[0] || f64::from(run.column_start) > visible[2] {
                return;
            }
            paint_run_rect(
                painter,
                viewport,
                run.column_start,
                end,
                run.row,
                color.gamma_multiply(0.45 * opacity),
                view,
            );
        }
        ReportMaskValues::Fractional { maximum, values } => {
            if *maximum == 0 {
                return;
            }
            for (offset, value) in values.iter().enumerate().filter(|(_, value)| **value > 0) {
                let Ok(offset) = u32::try_from(offset) else {
                    break;
                };
                let column = run.column_start.saturating_add(offset);
                if f64::from(column) < visible[0] || f64::from(column) > visible[2] {
                    continue;
                }
                let alpha = ((f32::from(*value) / f32::from(*maximum)) * 150.0 * opacity)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                paint_run_rect(
                    painter,
                    viewport,
                    column,
                    column.saturating_add(1),
                    run.row,
                    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
                    view,
                );
            }
        }
    }
}

fn paint_run_rect(
    painter: &egui::Painter,
    viewport: Rect,
    start: u32,
    end: u32,
    row: u32,
    color: Color32,
    view: CameraView,
) {
    let min = view.base_to_screen(viewport, Vec2::new(start as f32, row as f32));
    let max = view.base_to_screen(
        viewport,
        Vec2::new(end as f32, row.saturating_add(1) as f32),
    );
    painter.rect_filled(Rect::from_two_pos(min, max), 0.0, color);
}

fn screen_points(
    viewport: Rect,
    points: &[dicom_viewer_core::Point2],
    view: CameraView,
) -> Vec<Pos2> {
    points
        .iter()
        .map(|point| view.base_to_screen(viewport, Vec2::new(point.x as f32, point.y as f32)))
        .collect()
}

fn draw_groups(
    painter: &egui::Painter,
    viewport: Rect,
    groups: &[AnnotationGroup],
    imported: Option<(
        &dicom_viewer_core::AnnotationDocument,
        &DicomAnnotationContext,
    )>,
    opacity: f32,
    view: CameraView,
) {
    for group in groups {
        let rgb = dicom_cielab_to_srgb(group.recommended_display_cielab());
        let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]).gamma_multiply(opacity);
        let stroke = Stroke::new(1.35, color);
        match group.geometry() {
            AnnotationGeometry::Points(points) => {
                for point in points {
                    let screen =
                        view.base_to_screen(viewport, Vec2::new(point.x as f32, point.y as f32));
                    if viewport.contains(screen) {
                        painter.circle_filled(screen, 2.5, color);
                    }
                }
            }
            AnnotationGeometry::Polygons(polygons) => {
                for polygon in polygons {
                    let points = polygon
                        .iter()
                        .map(|point| {
                            view.base_to_screen(viewport, Vec2::new(point.x as f32, point.y as f32))
                        })
                        .collect::<Vec<_>>();
                    if points.len() >= 3 {
                        painter.add(Shape::closed_line(points, stroke));
                    }
                }
            }
            AnnotationGeometry::ReadOnly {
                graphic_type,
                coordinates,
                primitive_point_indices,
                coordinate_dimensions,
            } => {
                let Some((document, canonical_source)) = imported else {
                    continue;
                };
                let points = coordinates
                    .chunks_exact(*coordinate_dimensions)
                    .map(|coordinate| {
                        document
                            .canonical_level0_pixel(
                                canonical_source,
                                coordinate[0],
                                coordinate[1],
                                coordinate.get(2).copied(),
                            )
                            .map(|point| {
                                view.base_to_screen(
                                    viewport,
                                    Vec2::new(point.x as f32, point.y as f32),
                                )
                            })
                            .map_err(dicom_viewer_core::ViewerError::from)
                    })
                    .collect::<dicom_viewer_core::Result<Vec<_>>>();
                let Ok(points) = points else {
                    continue;
                };
                draw_read_only_primitives(
                    painter,
                    *graphic_type,
                    &points,
                    primitive_point_indices,
                    color,
                    stroke,
                );
            }
        }
    }
}

fn draw_read_only_primitives(
    painter: &egui::Painter,
    graphic_type: AnnotationGraphicType,
    points: &[Pos2],
    primitive_point_indices: &[u32],
    color: Color32,
    stroke: Stroke,
) {
    if graphic_type == AnnotationGraphicType::Point {
        for point in points {
            painter.circle_filled(*point, 2.5, color);
        }
        return;
    }
    for (start, end) in primitive_ranges(primitive_point_indices, points.len()) {
        let primitive = points[start..end].to_vec();
        if graphic_type == AnnotationGraphicType::Polygon && primitive.len() >= 3 {
            painter.add(Shape::closed_line(primitive, stroke));
        } else if primitive.len() >= 2 {
            painter.add(Shape::line(primitive, stroke));
        }
    }
}

fn primitive_ranges(indices: &[u32], point_count: usize) -> Vec<(usize, usize)> {
    if indices.is_empty() {
        return (point_count > 0)
            .then_some((0, point_count))
            .into_iter()
            .collect();
    }
    indices
        .iter()
        .enumerate()
        .filter_map(|(index, start)| {
            let start = usize::try_from(start.saturating_sub(1)).ok()?;
            let end = indices
                .get(index + 1)
                .and_then(|next| usize::try_from(next.saturating_sub(1)).ok())
                .unwrap_or(point_count);
            (start < end && end <= point_count).then_some((start, end))
        })
        .collect()
}

fn draw_fractional_seg(
    painter: &egui::Painter,
    viewport: Rect,
    document: &SegmentationDocument,
    opacity: f32,
    view: CameraView,
) {
    let Some(frames) = document.fractional_frames() else {
        return;
    };
    for frame in frames {
        let (width, height) = frame.dimensions();
        let base_x = frame.tile_col() * u32::from(width);
        let base_y = frame.tile_row() * u32::from(height);
        let frame_rect = Rect::from_two_pos(
            view.base_to_screen(viewport, Vec2::new(base_x as f32, base_y as f32)),
            view.base_to_screen(
                viewport,
                Vec2::new(
                    base_x.saturating_add(u32::from(width)) as f32,
                    base_y.saturating_add(u32::from(height)) as f32,
                ),
            ),
        );
        if !viewport.intersects(frame_rect) {
            continue;
        }
        let rgb = document
            .segment_for_number(frame.segment_number())
            .map(|segment| dicom_cielab_to_srgb(segment.recommended_display_cielab()))
            .unwrap_or([230, 159, 0]);
        let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        let maximum = frame.values().iter().copied().max().unwrap_or(0);
        let alpha = fractional_alpha(maximum, frame.maximum_fractional_value(), opacity);
        if alpha > 0 {
            painter.rect_filled(
                frame_rect,
                0.0,
                Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
            );
        }
    }
}

fn fractional_alpha(value: u16, maximum: u16, opacity: f32) -> u8 {
    if maximum == 0 {
        return 0;
    }
    ((f32::from(value) / f32::from(maximum)) * 150.0 * opacity.clamp(0.0, 1.0))
        .round()
        .clamp(0.0, 255.0) as u8
}
