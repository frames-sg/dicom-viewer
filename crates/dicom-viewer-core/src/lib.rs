#![forbid(unsafe_code)]

use std::num::NonZeroUsize;
use std::path::Path;

use wsi_rs::{
    CacheConfig, DecodeExecutionOptions, LevelIdx, Slide, SlideOpenOptions, TileOutputPreference,
    TilePixels, TileRequest, TileViewRequest,
};
pub use wsi_rs::{
    DicomIndexDiagnostic, DicomIndexMapping, DicomIndexOutcome, ReadCancellationToken, ReadControl,
    ReadDiagnosticSink,
};

mod color;
mod inspection;
mod model;
mod tile_output;

use inspection::{inspect_input, summarize_slide};
#[cfg(target_os = "macos")]
pub use model::MetalRenderTile;
pub use model::{
    ColorLut3d, ColorManagementMode, ColorManagementStatus, ColorManagementSummary,
    DicomInstanceSummary, LevelIndex, LevelInfo, LevelTileLayout, RenderTile, Result, RgbaTile,
    SourceKind, StudySummary, TileCoord, TileDecodeBackend, ViewerCacheBudgets, ViewerError,
    ViewerOpenOptions, ViewerStudy, DEFAULT_DISPLAY_TILE_SIZE,
};
use tile_output::{
    default_viewer_open_options, render_tile_from_pixels, rgba_tile_from_cpu_tile,
    rgba_tile_from_pixels, tile_output_config,
};

#[derive(Clone, Copy)]
enum BatchReadPolicy<'a> {
    Uncontrolled,
    Controlled(&'a ReadControl),
}

impl BatchReadPolicy<'_> {
    fn ensure_not_cancelled(self) -> Result<()> {
        match self {
            Self::Uncontrolled => Ok(()),
            Self::Controlled(control) => ensure_not_cancelled(control),
        }
    }
}

#[derive(Clone, Copy)]
struct ValidatedTileRequest {
    level_index: LevelIndex,
    coord: TileCoord,
    layout: LevelTileLayout,
}

enum TileBatchPlan {
    Empty,
    Regular(Vec<TileRequest>),
    Sequential(Vec<ValidatedTileRequest>),
}

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
        let (mut tile_decode_backend, mut render_tile_output, cpu_tile_output) =
            tile_output_config(&options);
        if !input.instances.is_empty() {
            render_tile_output = render_tile_output.without_adaptive_decode_route();
        }
        let cache_budgets = options.cache_budgets();
        let slide_options = SlideOpenOptions::deterministic()
            .with_cache_config(
                CacheConfig::deterministic()
                    .with_shared_tile_bytes(cache_budgets.shared_tile_bytes)
                    .with_display_tile_bytes(cache_budgets.display_tile_bytes),
            )
            .with_decode_execution_options(viewer_decode_execution_options());
        let slide = Slide::open_with_options(path, slide_options)?;
        let (mut summary, selected_view) =
            summarize_slide(path, &slide, input, tile_decode_backend)?;
        let color =
            color::ColorManagement::build(slide.dataset(), selected_view, tile_decode_backend);
        if color.force_cpu {
            tile_decode_backend = TileDecodeBackend::Cpu;
            render_tile_output = cpu_tile_output.clone();
            summary.tile_decode_backend = tile_decode_backend;
        }
        summary.color_management = color.summary;
        summary.warnings.extend(color.warnings);
        Ok(Self {
            slide,
            summary,
            render_tile_output,
            cpu_tile_output,
            selected_view,
            color_management: color.color,
        })
    }

    #[must_use]
    pub fn summary(&self) -> &StudySummary {
        &self.summary
    }

    /// Prepares source metadata needed to read one pyramid level without decoding pixels.
    ///
    /// Formats without deferred per-level metadata treat this as a cancellation-aware no-op.
    pub fn prepare_level_controlled(
        &self,
        level_index: LevelIndex,
        control: &wsi_rs::ReadControl,
    ) -> Result<()> {
        ensure_not_cancelled(control)?;
        self.level(level_index)?;
        self.slide.prepare_level_controlled(
            self.selected_view.scene,
            self.selected_view.series,
            LevelIdx::new(level_index.get()),
            control,
        )?;
        ensure_not_cancelled(control)
    }

    /// Reads one display tile as RGBA pixels.
    ///
    /// # Errors
    ///
    /// Returns an error when the level or tile coordinate is outside the slide
    /// summary, or when the underlying WSI reader cannot decode the tile.
    pub fn read_tile_rgba(&self, level_index: LevelIndex, coord: TileCoord) -> Result<RgbaTile> {
        let request = self.validate_tile_request(level_index, coord)?;
        self.read_validated_tile_rgba(request)
    }

    /// Reads display tiles as RGBA pixels using wsi-rs's batch tile path when possible.
    ///
    /// # Errors
    ///
    /// Returns an error when a requested level or tile coordinate is outside the
    /// slide summary, the backend returns the wrong number of tiles, or the
    /// underlying WSI reader cannot decode the batch.
    pub fn read_tiles_rgba(&self, requests: &[(LevelIndex, TileCoord)]) -> Result<Vec<RgbaTile>> {
        let policy = BatchReadPolicy::Uncontrolled;
        let plan = self.validate_tile_batch(requests)?;
        self.read_tiles_rgba_with_policy(plan, policy)
    }

    /// Reads RGBA tiles with cooperative cancellation between source reads.
    pub fn read_tiles_rgba_controlled(
        &self,
        requests: &[(LevelIndex, TileCoord)],
        control: &wsi_rs::ReadControl,
    ) -> Result<Vec<RgbaTile>> {
        let policy = BatchReadPolicy::Controlled(control);
        policy.ensure_not_cancelled()?;
        let plan = self.validate_tile_batch(requests)?;
        self.read_tiles_rgba_with_policy(plan, policy)
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
        let policy = BatchReadPolicy::Uncontrolled;
        let plan = self.validate_tile_batch(requests)?;
        self.read_tiles_for_render_with_policy(plan, policy)
    }

    /// Reads renderer tiles with cooperative cancellation between source reads.
    pub fn read_tiles_for_render_controlled(
        &self,
        requests: &[(LevelIndex, TileCoord)],
        control: &wsi_rs::ReadControl,
    ) -> Result<Vec<RenderTile>> {
        let policy = BatchReadPolicy::Controlled(control);
        policy.ensure_not_cancelled()?;
        let plan = self.validate_tile_batch(requests)?;
        self.read_tiles_for_render_with_policy(plan, policy)
    }

    fn validate_tile_request(
        &self,
        level_index: LevelIndex,
        coord: TileCoord,
    ) -> Result<ValidatedTileRequest> {
        let level = self.level(level_index)?;
        if !level.tile_layout.contains(coord) {
            return Err(ViewerError::InvalidInput(format!(
                "tile ({}, {}) is outside level {}",
                coord.col(),
                coord.row(),
                level.index
            )));
        }
        Ok(ValidatedTileRequest {
            level_index,
            coord,
            layout: level.tile_layout,
        })
    }

    fn validate_tile_batch(&self, requests: &[(LevelIndex, TileCoord)]) -> Result<TileBatchPlan> {
        if requests.is_empty() {
            return Ok(TileBatchPlan::Empty);
        }
        let requests = requests
            .iter()
            .map(|&(level_index, coord)| self.validate_tile_request(level_index, coord))
            .collect::<Result<Vec<_>>>()?;
        if requests
            .iter()
            .all(|request| matches!(request.layout, LevelTileLayout::Regular { .. }))
        {
            return requests
                .iter()
                .map(|request| {
                    build_tile_request(self.selected_view, request.level_index, request.coord)
                })
                .collect::<Result<Vec<_>>>()
                .map(TileBatchPlan::Regular);
        }
        Ok(TileBatchPlan::Sequential(requests))
    }

    fn read_tiles_rgba_with_policy(
        &self,
        plan: TileBatchPlan,
        policy: BatchReadPolicy<'_>,
    ) -> Result<Vec<RgbaTile>> {
        match plan {
            TileBatchPlan::Empty => Ok(Vec::new()),
            TileBatchPlan::Regular(requests) => self.read_regular_tiles_rgba(&requests, policy),
            TileBatchPlan::Sequential(requests) => {
                let mut tiles = Vec::with_capacity(requests.len());
                for request in requests {
                    policy.ensure_not_cancelled()?;
                    tiles.push(self.read_validated_tile_rgba(request)?);
                    policy.ensure_not_cancelled()?;
                }
                Ok(tiles)
            }
        }
    }

    fn read_tiles_for_render_with_policy(
        &self,
        plan: TileBatchPlan,
        policy: BatchReadPolicy<'_>,
    ) -> Result<Vec<RenderTile>> {
        match plan {
            TileBatchPlan::Empty => Ok(Vec::new()),
            TileBatchPlan::Regular(requests) => {
                self.read_regular_tiles_for_render(&requests, policy)
            }
            sequential @ TileBatchPlan::Sequential(_) => self
                .read_tiles_rgba_with_policy(sequential, policy)
                .map(|tiles| tiles.into_iter().map(RenderTile::Cpu).collect()),
        }
    }

    fn read_regular_tiles_rgba(
        &self,
        requests: &[TileRequest],
        policy: BatchReadPolicy<'_>,
    ) -> Result<Vec<RgbaTile>> {
        self.read_regular_tile_pixels(requests, &self.cpu_tile_output, policy)?
            .into_iter()
            .map(|tile| self.color_managed_rgba_from_pixels(tile))
            .collect()
    }

    fn read_regular_tiles_for_render(
        &self,
        requests: &[TileRequest],
        policy: BatchReadPolicy<'_>,
    ) -> Result<Vec<RenderTile>> {
        self.read_regular_tile_pixels(requests, &self.render_tile_output, policy)?
            .into_iter()
            .map(|tile| self.color_managed_render_from_pixels(tile))
            .collect()
    }

    fn read_regular_tile_pixels(
        &self,
        requests: &[TileRequest],
        output: &TileOutputPreference,
        policy: BatchReadPolicy<'_>,
    ) -> Result<Vec<TilePixels>> {
        let tiles = match policy {
            BatchReadPolicy::Uncontrolled => self.slide.read_tiles(requests, output.clone())?,
            BatchReadPolicy::Controlled(control) => {
                self.slide
                    .read_tiles_controlled(requests, output.clone(), control)?
            }
        };
        if tiles.len() != requests.len() {
            return Err(ViewerError::Unsupported(format!(
                "tile backend returned {} tiles for {} requests",
                tiles.len(),
                requests.len()
            )));
        }
        Ok(tiles)
    }

    fn read_validated_tile_rgba(&self, request: ValidatedTileRequest) -> Result<RgbaTile> {
        if matches!(request.layout, LevelTileLayout::Regular { .. }) {
            let tile_request =
                build_tile_request(self.selected_view, request.level_index, request.coord)?;
            let mut tiles =
                self.read_regular_tiles_rgba(&[tile_request], BatchReadPolicy::Uncontrolled)?;
            return tiles.pop().ok_or_else(|| {
                ViewerError::Unsupported("regular tile read returned no pixels".into())
            });
        }

        let (tile_width, tile_height) = request.layout.display_tile_size();
        let request = build_tile_view_request(
            self.selected_view,
            request.level_index,
            request.coord,
            tile_width,
            tile_height,
        )?;
        let tile = self.slide.read_display_tile(&request)?;
        let mut tile = rgba_tile_from_cpu_tile(tile)?;
        self.color_management.apply_rgba_tile(&mut tile);
        Ok(tile)
    }

    fn color_managed_rgba_from_pixels(&self, tile: wsi_rs::TilePixels) -> Result<RgbaTile> {
        let mut tile = rgba_tile_from_pixels(tile)?;
        self.color_management.apply_rgba_tile(&mut tile);
        Ok(tile)
    }

    fn color_managed_render_from_pixels(&self, tile: wsi_rs::TilePixels) -> Result<RenderTile> {
        render_tile_from_pixels(tile).map(|tile| self.color_management.prepare_render_tile(tile))
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

fn ensure_not_cancelled(control: &wsi_rs::ReadControl) -> Result<()> {
    if control.cancellation().is_cancelled() {
        Err(ViewerError::Wsi(wsi_rs::WsiError::Cancelled))
    } else {
        Ok(())
    }
}

fn viewer_decode_execution_options() -> DecodeExecutionOptions {
    let available = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    let configured = std::env::var("DICOM_VIEWER_JP2K_THREADS").ok();
    DecodeExecutionOptions::default().with_jp2k_cpu_threads(jp2k_cpu_decode_thread_budget(
        available,
        configured.as_deref(),
    ))
}

fn jp2k_cpu_decode_thread_budget(available: usize, configured: Option<&str>) -> NonZeroUsize {
    let threads = configured
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|threads| *threads > 0)
        .map_or_else(
            || available.saturating_sub(1).max(1),
            |threads| threads.min(available.max(1)),
        );
    NonZeroUsize::new(threads).unwrap_or(NonZeroUsize::MIN)
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
