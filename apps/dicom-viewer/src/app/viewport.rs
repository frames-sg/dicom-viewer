use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

use dicom_viewer_core::{LevelIndex, LevelInfo, StudySummary, TileCoord};
use eframe::egui::{vec2, Rect, Vec2};

use super::camera::MIN_ZOOM;
use super::tile::{TileKey, VisibleTile};

// Retain one candidate beyond the atomic scheduler limit. This keeps viewport
// enumeration bounded for hostile geometry while allowing the scheduler to
// observe, diagnose, and deterministically trim visible-only overflow after
// canonical deduplication.
#[cfg(test)]
const MAX_VISIBLE_TILES_PER_QUERY: usize = 8_193;

pub(super) fn level_by_index(summary: &StudySummary, index: LevelIndex) -> Option<&LevelInfo> {
    summary.levels.iter().find(|level| level.index == index)
}

pub(super) fn base_size(summary: &StudySummary) -> Option<Vec2> {
    let (width, height) = summary.canvas_dimensions;
    (width > 0 && height > 0).then(|| vec2(width as f32, height as f32))
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

pub(super) fn choose_render_level_with_hysteresis(
    summary: &StudySummary,
    zoom: f32,
    current: Option<LevelIndex>,
) -> Option<&LevelInfo> {
    const HYSTERESIS: f32 = 0.10;

    let preferred = choose_render_level(summary, zoom)?;
    let current = current.and_then(|index| level_by_index(summary, index));
    let Some(current) = current else {
        return Some(preferred);
    };
    if current.index == preferred.index
        || !current.downsample.is_finite()
        || current.downsample <= 0.0
    {
        return Some(preferred);
    }

    let zoom = zoom.abs().max(MIN_ZOOM);
    let keep_current = if current.downsample > preferred.downsample {
        // Zooming into a finer level: tolerate up to 10% upscale before
        // switching away from the coarser level.
        zoom * current.downsample as f32 <= 1.0 + HYSTERESIS
    } else {
        // Zooming out toward a coarser level: retain the finer level until
        // the new level is at least 90% of native sampling.
        zoom * preferred.downsample as f32 >= 1.0 - HYSTERESIS
    };
    Some(if keep_current { current } else { preferred })
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

pub(super) fn clamp_center_axis(center: f32, slide: f32) -> f32 {
    // The camera center is the user's focal point. Keep it on the slide while
    // allowing the viewport to show canvas beyond an edge, so zooming cannot
    // move that focus merely because the visible span changed.
    center.clamp(0.0, slide)
}

pub(super) fn screen_to_base(
    rect: Rect,
    screen: eframe::egui::Pos2,
    center_base: Vec2,
    zoom: f32,
) -> Vec2 {
    center_base + (screen - rect.center()) / zoom.max(MIN_ZOOM)
}

#[cfg(test)]
pub(super) fn visible_tiles(
    rect: Rect,
    level: &LevelInfo,
    generation: u64,
    center_base: Vec2,
    zoom: f32,
    margin: i64,
) -> Vec<VisibleTile> {
    visible_tiles_with_limit(
        rect,
        level,
        generation,
        center_base,
        zoom,
        margin,
        MAX_VISIBLE_TILES_PER_QUERY,
    )
}

pub(super) fn visible_tiles_with_limit(
    rect: Rect,
    level: &LevelInfo,
    generation: u64,
    center_base: Vec2,
    zoom: f32,
    margin: i64,
    limit: usize,
) -> Vec<VisibleTile> {
    if limit == 0 {
        return Vec::new();
    }
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
    let span_cols = end_col.saturating_sub(start_col);
    let span_rows = end_row.saturating_sub(start_row);
    let overflow = span_cols
        .checked_mul(span_rows)
        .is_none_or(|count| count > limit as u64);

    let candidates = if overflow {
        nearest_coordinates_in_bounds(
            start_col, end_col, start_row, end_row, center_col, center_row, limit,
        )
    } else {
        let mut coordinates =
            Vec::with_capacity(usize::try_from(span_cols.saturating_mul(span_rows)).unwrap_or(0));
        for row in start_row..end_row {
            for col in start_col..end_col {
                coordinates.push((tile_distance2(col, row, center_col, center_row), row, col));
            }
        }
        coordinates
    };
    let mut tiles = candidates
        .into_iter()
        .map(|(distance2, row, col)| VisibleTile {
            key: TileKey {
                generation,
                level: level.index,
                coord: TileCoord::new(col, row),
            },
            distance2,
        })
        .collect::<Vec<_>>();
    tiles.sort_by_key(|tile| (tile.distance2, tile.key.coord.row(), tile.key.coord.col()));
    tiles.truncate(limit);
    tiles
}

pub(super) fn nearest_grid_coordinates(
    cols: u64,
    rows: u64,
    limit: usize,
) -> Vec<(u128, u64, u64)> {
    nearest_coordinates_in_bounds(0, cols, 0, rows, cols / 2, rows / 2, limit)
}

fn nearest_coordinates_in_bounds(
    start_col: u64,
    end_col: u64,
    start_row: u64,
    end_row: u64,
    center_col: u64,
    center_row: u64,
    limit: usize,
) -> Vec<(u128, u64, u64)> {
    if limit == 0 || start_col >= end_col || start_row >= end_row {
        return Vec::new();
    }
    let seed_col = center_col.clamp(start_col, end_col - 1);
    let seed_row = center_row.clamp(start_row, end_row - 1);
    let mut pending = BinaryHeap::new();
    let mut discovered = HashSet::with_capacity(limit.saturating_mul(2));
    push_visible_coordinate(
        &mut pending,
        &mut discovered,
        seed_col,
        seed_row,
        center_col,
        center_row,
    );
    let mut coordinates = Vec::with_capacity(limit);
    while coordinates.len() < limit {
        let Some(Reverse((distance2, row, col))) = pending.pop() else {
            break;
        };
        coordinates.push((distance2, row, col));
        if col > start_col {
            push_visible_coordinate(
                &mut pending,
                &mut discovered,
                col - 1,
                row,
                center_col,
                center_row,
            );
        }
        if col.checked_add(1).is_some_and(|next| next < end_col) {
            push_visible_coordinate(
                &mut pending,
                &mut discovered,
                col + 1,
                row,
                center_col,
                center_row,
            );
        }
        if row > start_row {
            push_visible_coordinate(
                &mut pending,
                &mut discovered,
                col,
                row - 1,
                center_col,
                center_row,
            );
        }
        if row.checked_add(1).is_some_and(|next| next < end_row) {
            push_visible_coordinate(
                &mut pending,
                &mut discovered,
                col,
                row + 1,
                center_col,
                center_row,
            );
        }
    }
    coordinates
}

fn push_visible_coordinate(
    pending: &mut BinaryHeap<Reverse<(u128, u64, u64)>>,
    discovered: &mut HashSet<(u64, u64)>,
    col: u64,
    row: u64,
    center_col: u64,
    center_row: u64,
) {
    if discovered.insert((col, row)) {
        pending.push(Reverse((
            tile_distance2(col, row, center_col, center_row),
            row,
            col,
        )));
    }
}

fn tile_distance2(col: u64, row: u64, center_col: u64, center_row: u64) -> u128 {
    let dc = u128::from(col.abs_diff(center_col));
    let dr = u128::from(row.abs_diff(center_row));
    dc * dc + dr * dr
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
