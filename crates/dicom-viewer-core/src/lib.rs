#![forbid(unsafe_code)]

use std::num::NonZeroUsize;
use std::path::Path;

use wsi_rs::{
    CacheConfig, DecodeExecutionOptions, LevelIdx, Slide, SlideOpenOptions, TileRequest,
    TileViewRequest,
};

mod inspection;
mod model;
mod tile_output;

use inspection::{inspect_input, summarize_slide};
#[cfg(target_os = "macos")]
pub use model::MetalRenderTile;
pub use model::{
    DicomInstanceSummary, LevelIndex, LevelInfo, LevelTileLayout, RenderTile, Result, RgbaTile,
    SourceKind, StudySummary, TileCoord, TileDecodeBackend, ViewerError, ViewerOpenOptions,
    ViewerStudy, DEFAULT_DISPLAY_TILE_SIZE,
};
use tile_output::{
    default_viewer_open_options, render_tile_from_pixels, rgba_tile_from_cpu_tile,
    rgba_tile_from_pixels, tile_output_config,
};

impl ViewerStudy {
    /// Opens a local WSI file or a folder of DICOM instances.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is not a regular file or folder, metadata
    /// inspection fails, the input is unsupported by wsi-rs, or the slide
    /// cannot be summarized.
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_path_with_options(path, default_viewer_open_options()?)
    }

    /// Opens a local WSI with explicit renderer-output options.
    ///
    /// # Errors
    ///
    /// Returns the same input, metadata, and slide-open errors as [`Self::open_path`].
    pub fn open_path_with_options(
        path: impl AsRef<Path>,
        options: ViewerOpenOptions,
    ) -> Result<Self> {
        let path = path.as_ref();
        let input = inspect_input(path)?;
        let (tile_decode_backend, render_tile_output, cpu_tile_output) =
            tile_output_config(&options);
        let slide_options = SlideOpenOptions::deterministic()
            .with_cache_config(
                CacheConfig::deterministic()
                    .with_shared_tile_bytes(128 * 1024 * 1024)
                    .with_display_tile_bytes(32 * 1024 * 1024),
            )
            .with_decode_execution_options(viewer_decode_execution_options());
        let slide = Slide::open_with_options(path, slide_options)?;
        let (summary, selected_view) = summarize_slide(path, &slide, input, tile_decode_backend)?;
        Ok(Self {
            slide,
            summary,
            render_tile_output,
            cpu_tile_output,
            selected_view,
        })
    }

    #[must_use]
    pub fn summary(&self) -> &StudySummary {
        &self.summary
    }

    /// Reads one display tile as RGBA pixels.
    ///
    /// # Errors
    ///
    /// Returns an error when the level or tile coordinate is outside the slide
    /// summary, or when the underlying WSI reader cannot decode the tile.
    pub fn read_tile_rgba(&self, level_index: LevelIndex, coord: TileCoord) -> Result<RgbaTile> {
        self.validate_tile_request(level_index, coord)?;
        let level = self.level(level_index)?;
        if matches!(level.tile_layout, LevelTileLayout::Regular { .. }) {
            let mut tiles = self.read_tiles_rgba(&[(level_index, coord)])?;
            return tiles.pop().ok_or_else(|| {
                ViewerError::Unsupported("regular tile read returned no pixels".into())
            });
        }

        let (tile_width, tile_height) = level.tile_layout.display_tile_size();
        let request = build_tile_view_request(
            self.selected_view,
            level_index,
            coord,
            tile_width,
            tile_height,
        )?;
        let tile = self.slide.read_display_tile(&request)?;
        rgba_tile_from_cpu_tile(tile)
    }

    /// Reads display tiles as RGBA pixels using wsi-rs's batch tile path when possible.
    ///
    /// # Errors
    ///
    /// Returns an error when a requested level or tile coordinate is outside the
    /// slide summary, the backend returns the wrong number of tiles, or the
    /// underlying WSI reader cannot decode the batch.
    pub fn read_tiles_rgba(&self, requests: &[(LevelIndex, TileCoord)]) -> Result<Vec<RgbaTile>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        for &(level_index, coord) in requests {
            self.validate_tile_request(level_index, coord)?;
        }

        if requests.iter().all(|(level_index, _)| {
            self.level(*level_index)
                .is_ok_and(|level| matches!(level.tile_layout, LevelTileLayout::Regular { .. }))
        }) {
            let tile_requests = requests
                .iter()
                .map(|&(level_index, coord)| {
                    build_tile_request(self.selected_view, level_index, coord)
                })
                .collect::<Result<Vec<_>>>()?;
            let tiles = self
                .slide
                .read_tiles(&tile_requests, self.cpu_tile_output.clone())?;
            if tiles.len() != requests.len() {
                return Err(ViewerError::Unsupported(format!(
                    "tile backend returned {} tiles for {} requests",
                    tiles.len(),
                    requests.len()
                )));
            }
            return tiles.into_iter().map(rgba_tile_from_pixels).collect();
        }

        requests
            .iter()
            .map(|&(level_index, coord)| self.read_tile_rgba(level_index, coord))
            .collect()
    }

    /// Reads tiles using the output residency configured for the viewer renderer.
    ///
    /// Returned tiles preserve request order. Unsupported device output is reported
    /// rather than read back implicitly.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid requests, backend failures, or an unexpected
    /// renderer-output contract.
    pub fn read_tiles_for_render(
        &self,
        requests: &[(LevelIndex, TileCoord)],
    ) -> Result<Vec<RenderTile>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        for &(level_index, coord) in requests {
            self.validate_tile_request(level_index, coord)?;
        }
        if !requests.iter().all(|(level_index, _)| {
            self.level(*level_index)
                .is_ok_and(|level| matches!(level.tile_layout, LevelTileLayout::Regular { .. }))
        }) {
            return self
                .read_tiles_rgba(requests)
                .map(|tiles| tiles.into_iter().map(RenderTile::Cpu).collect());
        }

        let tile_requests = requests
            .iter()
            .map(|&(level_index, coord)| build_tile_request(self.selected_view, level_index, coord))
            .collect::<Result<Vec<_>>>()?;
        let tiles = self
            .slide
            .read_tiles(&tile_requests, self.render_tile_output.clone())?;
        if tiles.len() != requests.len() {
            return Err(ViewerError::Unsupported(format!(
                "tile backend returned {} tiles for {} requests",
                tiles.len(),
                requests.len()
            )));
        }
        tiles.into_iter().map(render_tile_from_pixels).collect()
    }

    fn validate_tile_request(&self, level_index: LevelIndex, coord: TileCoord) -> Result<()> {
        let level = self.level(level_index)?;
        if !level.tile_layout.contains(coord) {
            return Err(ViewerError::InvalidInput(format!(
                "tile ({}, {}) is outside level {}",
                coord.col(),
                coord.row(),
                level.index
            )));
        }
        Ok(())
    }

    fn level(&self, level_index: LevelIndex) -> Result<&LevelInfo> {
        self.summary
            .levels
            .iter()
            .find(|level| level.index == level_index)
            .ok_or_else(|| {
                ViewerError::InvalidInput(format!("level index {level_index} is not renderable"))
            })
    }
}

fn viewer_decode_execution_options() -> DecodeExecutionOptions {
    let available = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    DecodeExecutionOptions::default()
        .with_jp2k_cpu_threads(jp2k_cpu_decode_thread_budget(available))
}

fn jp2k_cpu_decode_thread_budget(available: usize) -> NonZeroUsize {
    NonZeroUsize::new(available.saturating_sub(1).max(1)).unwrap_or(NonZeroUsize::MIN)
}

fn build_tile_request(
    view: model::SelectedView,
    level: LevelIndex,
    coord: TileCoord,
) -> Result<TileRequest> {
    let (col, row) = coord.as_wsi_rs_i64()?;
    Ok(TileRequest::new(
        view.scene,
        view.series,
        LevelIdx::new(level.get()),
        col,
        row,
    )
    .with_plane(view.plane))
}

fn build_tile_view_request(
    view: model::SelectedView,
    level: LevelIndex,
    coord: TileCoord,
    tile_width: u32,
    tile_height: u32,
) -> Result<TileViewRequest> {
    let (col, row) = coord.as_wsi_rs_i64()?;
    Ok(TileViewRequest::new(
        view.scene,
        view.series,
        LevelIdx::new(level.get()),
        col,
        row,
        tile_width,
        tile_height,
    )
    .with_plane(view.plane))
}

#[cfg(test)]
mod tests;
