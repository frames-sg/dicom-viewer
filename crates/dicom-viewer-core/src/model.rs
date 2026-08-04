use std::error::Error as StdError;
use std::path::PathBuf;

use wsi_rs::{PlaneIdx, SceneId, SeriesId, Slide, TileOutputPreference};

pub type Result<T> = std::result::Result<T, ViewerError>;
pub const DEFAULT_DISPLAY_TILE_SIZE: u32 = 512;

#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unsupported input: {0}")]
    Unsupported(String),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("DICOM read error at {path}: {source}")]
    DicomRead {
        path: PathBuf,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
    #[error("WSI read error: {0}")]
    Wsi(#[from] wsi_rs::WsiError),
}

impl ViewerError {
    /// Returns whether this operation ended because its controlled read was cancelled.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Wsi(wsi_rs::WsiError::Cancelled))
    }

    /// Returns whether a renderer-facing CUDA tile failed at the checked host-download boundary.
    #[must_use]
    pub fn is_cuda_download_failure(&self) -> bool {
        #[cfg(feature = "cuda")]
        {
            return matches!(
                self,
                Self::Wsi(wsi_rs::WsiError::Codec { codec, .. })
                    if matches!(*codec, "cuda-jpeg-download" | "cuda-j2k-download")
            );
        }
        #[cfg(not(feature = "cuda"))]
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LevelIndex(u32);

impl LevelIndex {
    #[must_use]
    pub const fn from_u32(index: u32) -> Self {
        Self(index)
    }

    pub fn from_usize(index: usize) -> Result<Self> {
        let index = u32::try_from(index).map_err(|_| {
            ViewerError::InvalidInput(format!("level index {index} exceeds u32::MAX"))
        })?;
        Ok(Self(index))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl std::fmt::Display for LevelIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TileCoord {
    col: u64,
    row: u64,
}

impl TileCoord {
    #[must_use]
    pub const fn new(col: u64, row: u64) -> Self {
        Self { col, row }
    }

    #[must_use]
    pub const fn col(self) -> u64 {
        self.col
    }

    #[must_use]
    pub const fn row(self) -> u64 {
        self.row
    }

    pub(crate) fn as_wsi_rs_i64(self) -> Result<(i64, i64)> {
        let col = i64::try_from(self.col).map_err(|_| {
            ViewerError::InvalidInput(format!("tile column {} exceeds i64::MAX", self.col))
        })?;
        let row = i64::try_from(self.row).map_err(|_| {
            ViewerError::InvalidInput(format!("tile row {} exceeds i64::MAX", self.row))
        })?;
        Ok((col, row))
    }
}

#[derive(Debug)]
pub struct ViewerStudy {
    pub(crate) slide: Slide,
    pub(crate) summary: StudySummary,
    pub(crate) render_tile_output: TileOutputPreference,
    pub(crate) cpu_tile_output: TileOutputPreference,
    pub(crate) selected_view: SelectedView,
    pub(crate) color_management: crate::color::ColorManagement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestedTileOutput {
    Auto,
    CpuOnly,
}

/// Controls the residency requested for renderer-facing tile reads.
#[derive(Clone)]
pub struct ViewerOpenOptions {
    requested_tile_output: RequestedTileOutput,
    cache_budgets: ViewerCacheBudgets,
    #[cfg(target_os = "macos")]
    metal_device: Option<metal::Device>,
}

impl std::fmt::Debug for ViewerOpenOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("ViewerOpenOptions");
        debug.field("requested_tile_output", &self.requested_tile_output);
        debug.field("cache_budgets", &self.cache_budgets);
        #[cfg(target_os = "macos")]
        debug.field("has_metal_device", &self.metal_device.is_some());
        debug.finish()
    }
}

impl ViewerOpenOptions {
    /// Resolve the renderer-output preference from
    /// `DICOM_VIEWER_TILE_BACKEND` (`auto` or `cpu`).
    ///
    /// # Errors
    ///
    /// Returns an error for obsolete device-resident backend selections.
    pub fn from_environment() -> Result<Self> {
        crate::tile_output::default_viewer_open_options()
    }

    /// Prefer renderer-resident output when a compatible renderer device is supplied.
    #[must_use]
    pub const fn auto() -> Self {
        Self {
            requested_tile_output: RequestedTileOutput::Auto,
            cache_budgets: ViewerCacheBudgets::balanced(),
            #[cfg(target_os = "macos")]
            metal_device: None,
        }
    }

    /// Require CPU-resident pixels for renderer-facing and compatibility reads.
    #[must_use]
    pub const fn cpu_only() -> Self {
        Self {
            requested_tile_output: RequestedTileOutput::CpuOnly,
            cache_budgets: ViewerCacheBudgets::balanced(),
            #[cfg(target_os = "macos")]
            metal_device: None,
        }
    }

    #[must_use]
    pub const fn with_cache_budgets(mut self, cache_budgets: ViewerCacheBudgets) -> Self {
        self.cache_budgets = cache_budgets;
        self
    }

    #[must_use]
    pub const fn cache_budgets(&self) -> ViewerCacheBudgets {
        self.cache_budgets
    }

    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn with_metal_device(mut self, device: metal::Device) -> Self {
        self.metal_device = Some(device);
        self
    }

    #[cfg(any(target_os = "macos", feature = "cuda"))]
    pub(crate) const fn requests_cpu_only(&self) -> bool {
        matches!(self.requested_tile_output, RequestedTileOutput::CpuOnly)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn metal_device(&self) -> Option<&metal::Device> {
        self.metal_device.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerCacheBudgets {
    pub viewer_tile_bytes: u64,
    pub shared_tile_bytes: u64,
    pub display_tile_bytes: u64,
}

impl ViewerCacheBudgets {
    #[must_use]
    pub const fn new(
        viewer_tile_bytes: u64,
        shared_tile_bytes: u64,
        display_tile_bytes: u64,
    ) -> Self {
        Self {
            viewer_tile_bytes,
            shared_tile_bytes,
            display_tile_bytes,
        }
    }

    #[must_use]
    pub const fn balanced() -> Self {
        Self::new(256 * 1024 * 1024, 128 * 1024 * 1024, 32 * 1024 * 1024)
    }

    #[must_use]
    pub const fn large() -> Self {
        Self::new(512 * 1024 * 1024, 256 * 1024 * 1024, 64 * 1024 * 1024)
    }

    pub fn from_environment() -> Result<Self> {
        crate::tile_output::default_cache_budgets()
    }
}

impl Default for ViewerCacheBudgets {
    fn default() -> Self {
        Self::balanced()
    }
}

impl Default for ViewerOpenOptions {
    fn default() -> Self {
        Self::auto()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectedView {
    pub(crate) scene: SceneId,
    pub(crate) series: SeriesId,
    pub(crate) plane: PlaneIdx,
}

#[derive(Debug, Clone)]
pub struct StudySummary {
    pub source_path: PathBuf,
    pub source_kind: SourceKind,
    pub format_label: String,
    pub tile_decode_backend: TileDecodeBackend,
    pub file_count: usize,
    pub dicom_instance_count: usize,
    /// Canonical base-coordinate extent derived from every valid source level.
    pub canvas_dimensions: (u64, u64),
    pub levels: Vec<LevelInfo>,
    pub instances: Vec<DicomInstanceSummary>,
    pub warnings: Vec<String>,
    pub mpp: Option<(f64, f64)>,
    pub objective_power: Option<f64>,
    pub color_management: ColorManagementSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorManagementStatus {
    Unprofiled,
    Applied,
    MalformedProfile,
    LutValidationFailed,
}

impl std::fmt::Display for ColorManagementStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unprofiled => "unprofiled",
            Self::Applied => "applied",
            Self::MalformedProfile => "malformed profile",
            Self::LutValidationFailed => "LUT validation failed",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorManagementMode {
    Identity,
    CpuLittleCms,
    MetalLut65,
    CpuLutValidationFallback,
    UncorrectedMalformedProfile,
}

impl std::fmt::Display for ColorManagementMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Identity => "identity",
            Self::CpuLittleCms => "LittleCMS → sRGB",
            Self::MetalLut65 => "Metal 65³ LUT → sRGB",
            Self::CpuLutValidationFallback => "CPU LittleCMS fallback",
            Self::UncorrectedMalformedProfile => "uncorrected",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorManagementSummary {
    pub status: ColorManagementStatus,
    pub sha256: Option<String>,
    pub byte_size: Option<usize>,
    pub provenance: Option<String>,
    pub applied_mode: ColorManagementMode,
}

impl ColorManagementSummary {
    #[must_use]
    pub const fn unprofiled() -> Self {
        Self {
            status: ColorManagementStatus::Unprofiled,
            sha256: None,
            byte_size: None,
            provenance: None,
            applied_mode: ColorManagementMode::Identity,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    File,
    Folder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileDecodeBackend {
    Cpu,
    Metal,
    Cuda,
}

impl std::fmt::Display for TileDecodeBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cpu => formatter.write_str("CPU"),
            Self::Metal => formatter.write_str("Metal"),
            Self::Cuda => formatter.write_str("CUDA"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LevelInfo {
    pub index: LevelIndex,
    pub width: u64,
    pub height: u64,
    pub downsample: f64,
    pub tile_layout: LevelTileLayout,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LevelTileLayout {
    Regular {
        tile_width: u32,
        tile_height: u32,
        tiles_across: u64,
        tiles_down: u64,
    },
    WholeLevel {
        width: u64,
        height: u64,
        virtual_tile_width: u32,
        virtual_tile_height: u32,
    },
    Irregular {
        tile_advance: (f64, f64),
        tile_count: usize,
    },
}

#[derive(Debug, Clone)]
pub struct DicomInstanceSummary {
    pub path: PathBuf,
    pub sop_class_uid: String,
    pub series_instance_uid_present: bool,
    pub transfer_syntax_uid: String,
    pub image_type: Vec<String>,
    pub rows: Option<u32>,
    pub columns: Option<u32>,
    pub total_pixel_matrix_rows: Option<u32>,
    pub total_pixel_matrix_columns: Option<u32>,
    pub number_of_frames: Option<u32>,
    pub optical_path_count: Option<u32>,
    pub focal_plane_count: Option<u32>,
    pub concatenation_instance_count: Option<u32>,
    pub pixel_spacing: Option<(f64, f64)>,
    pub dimension_organization_type: Option<String>,
    pub samples_per_pixel: Option<u32>,
    pub photometric_interpretation: Option<String>,
    pub planar_configuration: Option<u32>,
    pub bits_allocated: Option<u32>,
    pub bits_stored: Option<u32>,
    pub high_bit: Option<u32>,
    pub pixel_representation: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct RgbaTile {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Tile pixels prepared for the viewer's renderer upload boundary.
#[derive(Debug)]
#[non_exhaustive]
pub enum RenderTile {
    Cpu(RgbaTile),
    #[cfg(target_os = "macos")]
    Metal(MetalRenderTile),
}

/// Validated Metal-resident pixels ready for the renderer interop boundary.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct MetalRenderTile {
    tile: wsi_rs::output::metal::MetalDeviceTile,
    color_lut: Option<std::sync::Arc<ColorLut3d>>,
}

#[derive(Debug)]
pub struct ColorLut3d {
    edge: u32,
    rgba: Vec<u8>,
    profile_sha256: String,
}

impl ColorLut3d {
    pub(crate) fn new(edge: u32, rgba: Vec<u8>, profile_sha256: String) -> Self {
        Self {
            edge,
            rgba,
            profile_sha256,
        }
    }

    /// Build a validated RGBA8 three-dimensional color lookup table.
    ///
    /// Values are ordered with red varying fastest, then green, then blue.
    pub fn from_rgba8(edge: u32, rgba: Vec<u8>, profile_sha256: impl Into<String>) -> Result<Self> {
        let expected_len = usize::try_from(edge)
            .ok()
            .and_then(|edge| edge.checked_pow(3))
            .and_then(|voxels| voxels.checked_mul(4))
            .ok_or_else(|| ViewerError::InvalidInput("color LUT dimensions overflow".into()))?;
        if edge < 2 || rgba.len() != expected_len {
            return Err(ViewerError::InvalidInput(format!(
                "color LUT edge {edge} requires {expected_len} RGBA bytes, got {}",
                rgba.len()
            )));
        }
        Ok(Self::new(edge, rgba, profile_sha256.into()))
    }

    #[must_use]
    pub const fn edge(&self) -> u32 {
        self.edge
    }

    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    #[must_use]
    pub fn profile_sha256(&self) -> &str {
        &self.profile_sha256
    }
}

#[cfg(target_os = "macos")]
impl MetalRenderTile {
    pub(crate) fn new(tile: wsi_rs::output::metal::MetalDeviceTile) -> Result<Self> {
        tile.validated_resident_image()?;
        Ok(Self {
            tile,
            color_lut: None,
        })
    }

    pub(crate) fn with_color_lut(mut self, color_lut: Option<std::sync::Arc<ColorLut3d>>) -> Self {
        self.color_lut = color_lut;
        self
    }

    #[must_use]
    pub fn color_lut(&self) -> Option<&ColorLut3d> {
        self.color_lut.as_deref()
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.tile.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.tile.height
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.tile
            .validated_resident_image()
            .map_or(0, j2k_metal_support::ResidentMetalImage::byte_len)
    }

    /// Borrows the immutable resident image for an audited renderer operation.
    ///
    /// # Errors
    ///
    /// Returns an error if compatibility metadata no longer matches the image.
    pub fn resident_image(&self) -> Result<&j2k_metal_support::ResidentMetalImage> {
        self.tile.validated_resident_image().map_err(Into::into)
    }
}
impl LevelTileLayout {
    #[must_use]
    pub fn display_tile_size(self) -> (u32, u32) {
        match self {
            Self::Regular {
                tile_width,
                tile_height,
                ..
            } => (tile_width.max(1), tile_height.max(1)),
            Self::WholeLevel { .. } => (DEFAULT_DISPLAY_TILE_SIZE, DEFAULT_DISPLAY_TILE_SIZE),
            Self::Irregular { tile_advance, .. } => (
                bounded_tile_extent(tile_advance.0),
                bounded_tile_extent(tile_advance.1),
            ),
        }
    }

    #[must_use]
    pub fn grid_size(self) -> Option<(u64, u64)> {
        match self {
            Self::Regular {
                tiles_across,
                tiles_down,
                ..
            } => Some((tiles_across, tiles_down)),
            Self::WholeLevel { width, height, .. } => Some((
                width.div_ceil(u64::from(DEFAULT_DISPLAY_TILE_SIZE)),
                height.div_ceil(u64::from(DEFAULT_DISPLAY_TILE_SIZE)),
            )),
            Self::Irregular { .. } => None,
        }
    }

    pub(crate) fn contains(self, coord: TileCoord) -> bool {
        self.grid_size()
            .is_some_and(|(cols, rows)| coord.col() < cols && coord.row() < rows)
    }
}

fn bounded_tile_extent(value: f64) -> u32 {
    if !value.is_finite() {
        return 1;
    }
    let rounded = value.ceil().max(1.0).min(f64::from(u32::MAX));
    rounded as u32
}
