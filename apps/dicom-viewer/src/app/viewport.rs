use dicom_viewer_core::{LevelIndex, LevelInfo, StudySummary, TileCoord};
use eframe::egui::{vec2, Rect, Vec2};

use super::camera::MIN_ZOOM;
use super::tile::{TileKey, VisibleTile};

pub(super) fn base_level(summary: &StudySummary) -> Option<&LevelInfo> {
    summary.levels.first()
}

pub(super) fn level_by_index(summary: &StudySummary, index: LevelIndex) -> Option<&LevelInfo> {
    summary.levels.iter().find(|level| level.index == index)
}

pub(super) fn base_size(summary: &StudySummary) -> Option<Vec2> {
    base_level(summary).map(|level| vec2(level.width.max(1) as f32, level.height.max(1) as f32))
}

pub(super) fn base_center(summary: &StudySummary) -> Vec2 {
    base_size(summary).map_or(Vec2::ZERO, |size| size * 0.5)
}

pub(super) fn base_contains_point(summary: &StudySummary, point: Vec2) -> bool {
    base_size(summary).is_some_and(|size| {
        point.x >= 0.0 && point.y >= 0.0 && point.x < size.x && point.y < size.y
    })
}

pub(super) fn choose_render_level(summary: &StudySummary, zoom: f32) -> Option<&LevelInfo> {
    let zoom = zoom.abs().max(MIN_ZOOM);
    summary
        .levels
        .iter()
        .filter(|level| level.downsample.is_finite() && level.downsample > 0.0)
        .filter(|level| zoom * level.downsample as f32 <= 1.0 + f32::EPSILON)
        .max_by(|a, b| {
            a.downsample
                .total_cmp(&b.downsample)
                .then_with(|| b.index.cmp(&a.index))
        })
        .or_else(|| {
            summary
                .levels
                .iter()
                .filter(|level| level.downsample.is_finite() && level.downsample > 0.0)
                .min_by(|a, b| {
                    a.downsample
                        .total_cmp(&b.downsample)
                        .then_with(|| a.index.cmp(&b.index))
                })
        })
}

pub(super) fn choose_display_level_index(
    held_level: Option<LevelIndex>,
    target_level: LevelIndex,
    target_pending: usize,
    held_pending: usize,
) -> LevelIndex {
    let Some(held_level) = held_level else {
        return target_level;
    };
    if held_level == target_level {
        return target_level;
    }
    if target_pending > 0 && held_pending == 0 {
        held_level
    } else {
        target_level
    }
}

pub(super) fn clamp_center_axis(center: f32, visible: f32, slide: f32) -> f32 {
    if visible >= slide {
        slide * 0.5
    } else {
        center.clamp(visible * 0.5, slide - visible * 0.5)
    }
}

pub(super) fn screen_to_base(
    rect: Rect,
    screen: eframe::egui::Pos2,
    center_base: Vec2,
    zoom: f32,
) -> Vec2 {
    center_base + (screen - rect.center()) / zoom.max(MIN_ZOOM)
}

pub(super) fn visible_tiles(
    rect: Rect,
    level: &LevelInfo,
    generation: u64,
    center_base: Vec2,
    zoom: f32,
    margin: i64,
) -> Vec<VisibleTile> {
    let Some((cols, rows)) = level.tile_layout.grid_size() else {
        return Vec::new();
    };
    let downsample = level.downsample as f32;
    if !(downsample.is_finite() && downsample > 0.0) {
        return Vec::new();
    }
    let (tile_w, tile_h) = level.tile_layout.display_tile_size();
    let tile_w = u64::from(tile_w.max(1));
    let tile_h = u64::from(tile_h.max(1));
    let base_min = center_base - rect.size() / (2.0 * zoom.max(MIN_ZOOM));
    let base_max = center_base + rect.size() / (2.0 * zoom.max(MIN_ZOOM));
    let min_x = (base_min.x / downsample).floor().max(0.0);
    let min_y = (base_min.y / downsample).floor().max(0.0);
    let max_x = (base_max.x / downsample)
        .ceil()
        .min(level.width as f32)
        .max(0.0);
    let max_y = (base_max.y / downsample)
        .ceil()
        .min(level.height as f32)
        .max(0.0);

    let margin = u64::try_from(margin.max(0)).unwrap_or(0);
    let start_col = ((min_x as u64) / tile_w).saturating_sub(margin).min(cols);
    let start_row = ((min_y as u64) / tile_h).saturating_sub(margin).min(rows);
    let end_col = ((max_x as u64).div_ceil(tile_w))
        .saturating_add(margin)
        .min(cols);
    let end_row = ((max_y as u64).div_ceil(tile_h))
        .saturating_add(margin)
        .min(rows);
    let center_col = start_col.midpoint(end_col);
    let center_row = start_row.midpoint(end_row);

    let mut tiles = Vec::new();
    for row in start_row..end_row {
        for col in start_col..end_col {
            let dc = u128::from(col.abs_diff(center_col));
            let dr = u128::from(row.abs_diff(center_row));
            tiles.push(VisibleTile {
                key: TileKey {
                    generation,
                    level: level.index,
                    coord: TileCoord::new(col, row),
                },
                distance2: dc * dc + dr * dr,
            });
        }
    }
    tiles.sort_by_key(|tile| (tile.distance2, tile.key.coord.row(), tile.key.coord.col()));
    tiles
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CanvasView {
    pub(super) rect: Rect,
    pub(super) center_base: Vec2,
    pub(super) zoom: f32,
}

pub(super) fn tile_screen_rect(
    view: CanvasView,
    level: &LevelInfo,
    coord: TileCoord,
    width: u32,
    height: u32,
) -> Rect {
    let (tile_w, tile_h) = level.tile_layout.display_tile_size();
    let downsample = level.downsample as f32;
    let base_origin = vec2(
        coord.col() as f32 * tile_w as f32 * downsample,
        coord.row() as f32 * tile_h as f32 * downsample,
    );
    Rect::from_min_size(
        view.rect.center() + (base_origin - view.center_base) * view.zoom,
        vec2(
            width as f32 * downsample * view.zoom,
            height as f32 * downsample * view.zoom,
        ),
    )
}
