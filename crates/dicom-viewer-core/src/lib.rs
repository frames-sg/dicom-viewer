use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;
use std::path::{Path, PathBuf};

use dicom_core::Tag;
use dicom_dictionary_std::{tags, uids};
use dicom_object::{DefaultDicomObject, OpenFileOptions};
use wsi_rs::{
    CacheConfig, DeviceTile, LevelIdx, PixelFormat, SceneId, SeriesId, Slide, SlideOpenOptions,
    TileLayout, TileOutputPreference, TilePixels, TileRequest, TileViewRequest,
};

pub type Result<T> = std::result::Result<T, ViewerError>;
pub const DEFAULT_DISPLAY_TILE_SIZE: u32 = 512;
const TILE_BACKEND_ENV: &str = "DICOM_VIEWER_TILE_BACKEND";

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

    fn as_wsi_rs_i64(self) -> Result<(i64, i64)> {
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
    slide: Slide,
    summary: StudySummary,
    tile_output: TileOutputPreference,
}

#[derive(Debug, Clone)]
pub struct StudySummary {
    pub source_path: PathBuf,
    pub source_kind: SourceKind,
    pub format_label: String,
    pub tile_decode_backend: TileDecodeBackend,
    pub file_count: usize,
    pub dicom_instance_count: usize,
    pub levels: Vec<LevelInfo>,
    pub instances: Vec<DicomInstanceSummary>,
    pub warnings: Vec<String>,
    pub mpp: Option<(f64, f64)>,
    pub objective_power: Option<f64>,
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

impl ViewerStudy {
    /// Opens a local WSI file or a folder of DICOM instances.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is not a regular file or folder, metadata
    /// inspection fails, the input is unsupported by wsi-rs, or the slide
    /// cannot be summarized.
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let input = inspect_input(path)?;
        let (tile_decode_backend, tile_output) = default_tile_decode_config()?;
        let options = SlideOpenOptions::deterministic().with_cache_config(
            CacheConfig::deterministic()
                .with_shared_tile_bytes(128 * 1024 * 1024)
                .with_display_tile_bytes(32 * 1024 * 1024),
        );
        let slide = Slide::open_with_options(path, options)?;
        let summary = summarize_slide(path, &slide, input, tile_decode_backend)?;
        Ok(Self {
            slide,
            summary,
            tile_output,
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
        if matches!(
            self.summary.levels[level_index.as_usize()].tile_layout,
            LevelTileLayout::Regular { .. }
        ) {
            let mut tiles = self.read_tiles_rgba(&[(level_index, coord)])?;
            return tiles.pop().ok_or_else(|| {
                ViewerError::Unsupported("regular tile read returned no pixels".into())
            });
        }

        let (tile_width, tile_height) = self.summary.levels[level_index.as_usize()]
            .tile_layout
            .display_tile_size();
        let (col, row) = coord.as_wsi_rs_i64()?;
        let request = TileViewRequest::new(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(level_index.get()),
            col,
            row,
            tile_width,
            tile_height,
        );
        let tile = self.slide.read_display_tile(&request)?;
        rgba_tile_from_cpu_tile(tile)
    }

    /// Reads display tiles as RGBA pixels using wsi-rs's batch tile path when possible.
    ///
    /// # Errors
    ///
    /// Returns an error when a requested level or tile coordinate is outside the
    /// slide summary, the underlying reader cannot decode the batch, or a
    /// non-CPU tile is returned for a CPU request.
    pub fn read_tiles_rgba(&self, requests: &[(LevelIndex, TileCoord)]) -> Result<Vec<RgbaTile>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        for &(level_index, coord) in requests {
            self.validate_tile_request(level_index, coord)?;
        }

        if requests.iter().all(|(level_index, _)| {
            matches!(
                self.summary.levels[level_index.as_usize()].tile_layout,
                LevelTileLayout::Regular { .. }
            )
        }) {
            let tile_requests = requests
                .iter()
                .map(|&(level_index, coord)| {
                    let (col, row) = coord.as_wsi_rs_i64()?;
                    Ok(TileRequest::new(
                        SceneId::new(0),
                        SeriesId::new(0),
                        LevelIdx::new(level_index.get()),
                        col,
                        row,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            let tiles = self
                .slide
                .read_tiles(&tile_requests, self.tile_output.clone())?;
            return tiles.into_iter().map(rgba_tile_from_pixels).collect();
        }

        requests
            .iter()
            .map(|&(level_index, coord)| self.read_tile_rgba(level_index, coord))
            .collect()
    }

    fn validate_tile_request(&self, level_index: LevelIndex, coord: TileCoord) -> Result<()> {
        let level = self
            .summary
            .levels
            .get(level_index.as_usize())
            .ok_or_else(|| {
                ViewerError::InvalidInput(format!("level index {level_index} is out of range"))
            })?;
        if level.index != level_index {
            return Err(ViewerError::InvalidInput(format!(
                "level index {level_index} does not match summary level {}",
                level.index
            )));
        }
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
}

fn rgba_tile_from_cpu_tile(tile: wsi_rs::CpuTile) -> Result<RgbaTile> {
    let image = tile.into_rgba()?;
    Ok(RgbaTile {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

fn rgba_tile_from_pixels(tile: TilePixels) -> Result<RgbaTile> {
    match tile {
        TilePixels::Cpu(tile) => rgba_tile_from_cpu_tile(tile),
        TilePixels::Device(tile) => rgba_tile_from_device_tile(tile),
        #[allow(unreachable_patterns)]
        _ => Err(ViewerError::Unsupported(
            "unsupported tile pixel output variant".into(),
        )),
    }
}

fn rgba_tile_from_device_tile(tile: DeviceTile) -> Result<RgbaTile> {
    match tile {
        #[cfg(target_os = "macos")]
        DeviceTile::Metal(tile) => rgba_tile_from_metal_tile(tile),
        #[cfg(feature = "cuda")]
        DeviceTile::Cuda(tile) => rgba_tile_from_cuda_tile(tile),
        #[allow(unreachable_patterns)]
        _ => Err(ViewerError::Unsupported(
            "device tile readback is not available for this backend".into(),
        )),
    }
}

#[cfg(target_os = "macos")]
fn rgba_tile_from_metal_tile(tile: wsi_rs::output::metal::MetalDeviceTile) -> Result<RgbaTile> {
    let row_bytes = checked_row_bytes(tile.width, tile.format)?;
    validate_device_pitch(tile.pitch_bytes, row_bytes)?;
    let byte_len = checked_strided_len(tile.height, tile.pitch_bytes)?;
    let bytes = match tile.storage {
        wsi_rs::output::metal::MetalDeviceStorage::Buffer {
            buffer,
            byte_offset,
        } => {
            let end = byte_offset
                .checked_add(byte_len)
                .ok_or_else(|| ViewerError::Unsupported("Metal tile byte range overflow".into()))?;
            let buffer_len = usize::try_from(buffer.length()).map_err(|_| {
                ViewerError::Unsupported("Metal tile buffer length exceeds usize".into())
            })?;
            if end > buffer_len {
                return Err(ViewerError::Unsupported(format!(
                    "Metal tile byte range {}..{} exceeds buffer length {}",
                    byte_offset, end, buffer_len
                )));
            }
            let contents = buffer.contents();
            if contents.is_null() {
                return Err(ViewerError::Unsupported(
                    "Metal tile buffer is not CPU-visible for readback".into(),
                ));
            }
            // SAFETY: The Metal buffer is retained by `buffer`, the range was
            // bounds-checked above, and we copy the bytes before returning.
            unsafe {
                std::slice::from_raw_parts(contents.cast::<u8>().add(byte_offset), byte_len)
                    .to_vec()
            }
        }
        #[allow(unreachable_patterns)]
        _ => {
            return Err(ViewerError::Unsupported(
                "unsupported Metal tile storage variant".into(),
            ));
        }
    };
    rgba_tile_from_strided_bytes(
        tile.width,
        tile.height,
        tile.pitch_bytes,
        tile.format,
        &bytes,
    )
}

#[cfg(feature = "cuda")]
fn rgba_tile_from_cuda_tile(tile: wsi_rs::output::cuda::CudaDeviceTile) -> Result<RgbaTile> {
    let row_bytes = checked_row_bytes(tile.width, tile.format)?;
    validate_device_pitch(tile.pitch_bytes, row_bytes)?;
    let byte_len = checked_strided_len(tile.height, tile.pitch_bytes)?;
    let mut bytes = vec![0u8; byte_len];
    match &tile.storage {
        wsi_rs::output::cuda::CudaDeviceStorage::JpegSurface { surface } => surface
            .download_into(&mut bytes, tile.pitch_bytes)
            .map_err(|err| ViewerError::Unsupported(format!("CUDA JPEG readback failed: {err}")))?,
        wsi_rs::output::cuda::CudaDeviceStorage::J2kSurface { surface } => surface
            .download_into(&mut bytes, tile.pitch_bytes)
            .map_err(|err| ViewerError::Unsupported(format!("CUDA J2K readback failed: {err}")))?,
        #[allow(unreachable_patterns)]
        _ => {
            return Err(ViewerError::Unsupported(
                "unsupported CUDA tile storage variant".into(),
            ));
        }
    }
    rgba_tile_from_strided_bytes(
        tile.width,
        tile.height,
        tile.pitch_bytes,
        tile.format,
        &bytes,
    )
}

fn rgba_tile_from_strided_bytes(
    width: u32,
    height: u32,
    pitch_bytes: usize,
    format: PixelFormat,
    bytes: &[u8],
) -> Result<RgbaTile> {
    let width_usize = width as usize;
    let height_usize = height as usize;
    let row_bytes = checked_row_bytes(width, format)?;
    let byte_len = checked_strided_len(height, pitch_bytes)?;
    if bytes.len() < byte_len {
        return Err(ViewerError::Unsupported(format!(
            "device tile readback returned {} bytes, expected at least {}",
            bytes.len(),
            byte_len
        )));
    }

    let pixel_count = width_usize
        .checked_mul(height_usize)
        .ok_or_else(|| ViewerError::Unsupported("device tile pixel count overflow".into()))?;
    let rgba_len = pixel_count
        .checked_mul(4)
        .ok_or_else(|| ViewerError::Unsupported("device tile RGBA length overflow".into()))?;
    let mut rgba = Vec::with_capacity(rgba_len);
    for row in 0..height_usize {
        let row_start = row
            .checked_mul(pitch_bytes)
            .ok_or_else(|| ViewerError::Unsupported("device tile row offset overflow".into()))?;
        let row_end = row_start
            .checked_add(row_bytes)
            .ok_or_else(|| ViewerError::Unsupported("device tile row end overflow".into()))?;
        let row = &bytes[row_start..row_end];
        match format {
            PixelFormat::Rgb8 => {
                for pixel in row.chunks_exact(3) {
                    rgba.extend_from_slice(pixel);
                    rgba.push(255);
                }
            }
            PixelFormat::Rgba8 => rgba.extend_from_slice(row),
            PixelFormat::Gray8 => {
                for &value in row {
                    rgba.extend_from_slice(&[value, value, value, 255]);
                }
            }
            _ => {
                return Err(ViewerError::Unsupported(format!(
                    "device tile readback cannot display {format:?} without windowing"
                )));
            }
        }
    }

    Ok(RgbaTile {
        width,
        height,
        rgba,
    })
}

fn checked_row_bytes(width: u32, format: PixelFormat) -> Result<usize> {
    (width as usize)
        .checked_mul(format.bytes_per_pixel())
        .ok_or_else(|| ViewerError::Unsupported("device tile row byte count overflow".into()))
}

fn checked_strided_len(height: u32, pitch_bytes: usize) -> Result<usize> {
    (height as usize)
        .checked_mul(pitch_bytes)
        .ok_or_else(|| ViewerError::Unsupported("device tile byte count overflow".into()))
}

fn validate_device_pitch(pitch_bytes: usize, row_bytes: usize) -> Result<()> {
    if pitch_bytes < row_bytes {
        return Err(ViewerError::Unsupported(format!(
            "device tile pitch {pitch_bytes} is smaller than row byte count {row_bytes}"
        )));
    }
    Ok(())
}

fn default_tile_decode_config() -> Result<(TileDecodeBackend, TileOutputPreference)> {
    let requested = std::env::var(TILE_BACKEND_ENV).unwrap_or_else(|_| "auto".into());
    match requested.to_ascii_lowercase().as_str() {
        "auto" => Ok(auto_tile_decode_config()),
        "cpu" => Ok((TileDecodeBackend::Cpu, TileOutputPreference::cpu())),
        "metal" => metal_tile_decode_config(),
        "cuda" => cuda_tile_decode_config(),
        other => Err(ViewerError::InvalidInput(format!(
            "{TILE_BACKEND_ENV} must be auto, cpu, metal, or cuda; got {other:?}"
        ))),
    }
}

fn auto_tile_decode_config() -> (TileDecodeBackend, TileOutputPreference) {
    if let Ok(config) = metal_tile_decode_config() {
        return config;
    }
    if let Ok(config) = cuda_tile_decode_config() {
        return config;
    }
    (TileDecodeBackend::Cpu, TileOutputPreference::cpu())
}

#[cfg(target_os = "macos")]
fn metal_tile_decode_config() -> Result<(TileDecodeBackend, TileOutputPreference)> {
    let device = metal::Device::system_default().ok_or_else(|| {
        ViewerError::Unsupported(
            "Metal decode requested but no default Metal device is available".into(),
        )
    })?;
    let sessions = wsi_rs::output::metal::MetalBackendSessions::new(device);
    Ok((
        TileDecodeBackend::Metal,
        TileOutputPreference::prefer_device_auto_with_metal_and_compressed_decode(sessions),
    ))
}

#[cfg(not(target_os = "macos"))]
fn metal_tile_decode_config() -> Result<(TileDecodeBackend, TileOutputPreference)> {
    Err(ViewerError::Unsupported(
        "Metal decode requested but this build does not include Metal support".into(),
    ))
}

#[cfg(feature = "cuda")]
fn cuda_tile_decode_config() -> Result<(TileDecodeBackend, TileOutputPreference)> {
    let sessions = wsi_rs::output::cuda::CudaBackendSessions::new();
    Ok((
        TileDecodeBackend::Cuda,
        TileOutputPreference::prefer_device_auto_with_cuda_and_compressed_decode(sessions),
    ))
}

#[cfg(not(feature = "cuda"))]
fn cuda_tile_decode_config() -> Result<(TileDecodeBackend, TileOutputPreference)> {
    Err(ViewerError::Unsupported(
        "CUDA decode requested but this build does not include CUDA support".into(),
    ))
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

    fn contains(self, coord: TileCoord) -> bool {
        self.grid_size()
            .is_some_and(|(cols, rows)| coord.col() < cols && coord.row() < rows)
            || matches!(self, Self::Irregular { .. })
    }
}

fn bounded_tile_extent(value: f64) -> u32 {
    if !value.is_finite() {
        return 1;
    }
    let rounded = value.ceil().max(1.0).min(f64::from(u32::MAX));
    rounded as u32
}

#[derive(Debug)]
struct InputInspection {
    source_kind: SourceKind,
    file_count: usize,
    instances: Vec<DicomInstanceSummary>,
    warnings: Vec<String>,
}

fn summarize_slide(
    path: &Path,
    slide: &Slide,
    input: InputInspection,
    tile_decode_backend: TileDecodeBackend,
) -> Result<StudySummary> {
    let dataset = slide.dataset();
    let series = dataset
        .scenes
        .first()
        .and_then(|scene| scene.series.first())
        .ok_or_else(|| ViewerError::Unsupported("WSI dataset has no image series".into()))?;

    let levels = series
        .levels
        .iter()
        .enumerate()
        .map(|(index, level)| {
            let index = LevelIndex::from_usize(index)?;
            Ok(LevelInfo {
                index,
                width: level.dimensions.0,
                height: level.dimensions.1,
                downsample: level.downsample,
                tile_layout: match &level.tile_layout {
                    TileLayout::Regular {
                        tile_width,
                        tile_height,
                        tiles_across,
                        tiles_down,
                    } => LevelTileLayout::Regular {
                        tile_width: *tile_width,
                        tile_height: *tile_height,
                        tiles_across: *tiles_across,
                        tiles_down: *tiles_down,
                    },
                    TileLayout::WholeLevel {
                        width,
                        height,
                        virtual_tile_width,
                        virtual_tile_height,
                    } => LevelTileLayout::WholeLevel {
                        width: *width,
                        height: *height,
                        virtual_tile_width: *virtual_tile_width,
                        virtual_tile_height: *virtual_tile_height,
                    },
                    TileLayout::Irregular {
                        tile_advance,
                        tiles,
                        ..
                    } => LevelTileLayout::Irregular {
                        tile_advance: *tile_advance,
                        tile_count: tiles.len(),
                    },
                    _ => LevelTileLayout::Irregular {
                        tile_advance: (1.0, 1.0),
                        tile_count: 0,
                    },
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;

    if levels.is_empty() {
        return Err(ViewerError::Unsupported("WSI dataset has no levels".into()));
    }

    let format_label = format_label(
        path,
        dataset.properties.vendor(),
        !input.instances.is_empty(),
    );
    let mut warnings = input.warnings;
    warnings.extend(build_fact_warnings(&input.instances, &levels));

    Ok(StudySummary {
        source_path: path.to_path_buf(),
        source_kind: input.source_kind,
        format_label,
        tile_decode_backend,
        file_count: input.file_count,
        dicom_instance_count: input.instances.len(),
        levels,
        instances: input.instances,
        warnings,
        mpp: dataset.properties.mpp(),
        objective_power: dataset.properties.objective_power(),
    })
}

fn inspect_input(path: &Path) -> Result<InputInspection> {
    let metadata = std::fs::metadata(path).map_err(|source| ViewerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let source_kind = if metadata.is_dir() {
        SourceKind::Folder
    } else if metadata.is_file() {
        SourceKind::File
    } else {
        return Err(ViewerError::InvalidInput(format!(
            "{} is neither a file nor a folder",
            path.display()
        )));
    };

    let candidates = candidate_paths(path, source_kind)?;
    let file_count = candidates.len();
    let mut instances = Vec::new();
    let mut warnings = Vec::new();
    let mut series_instance_uids = BTreeSet::new();
    for candidate in candidates {
        let likely_dicom = likely_dicom_path(&candidate);
        match inspect_dicom_instance(&candidate) {
            Ok(Some((instance, series_instance_uid))) => {
                if let Some(uid) = series_instance_uid {
                    series_instance_uids.insert(uid);
                }
                instances.push(instance);
            }
            Ok(None) => {
                if likely_dicom {
                    warnings.push(format!(
                        "ignored non-WSI DICOM file {}",
                        candidate.file_name_display()
                    ));
                }
            }
            Err(err) => {
                if likely_dicom {
                    warnings.push(format!(
                        "ignored unreadable DICOM candidate {}: {err}",
                        candidate.file_name_display()
                    ));
                }
            }
        }
    }

    instances.sort_by(|a, b| a.path.cmp(&b.path));
    if series_instance_uids.len() > 1 {
        warnings.push(format!(
            "folder contains {} distinct DICOM series; open one series folder at a time for deterministic verification",
            series_instance_uids.len()
        ));
    }
    Ok(InputInspection {
        source_kind,
        file_count,
        instances,
        warnings,
    })
}

fn likely_dicom_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "dcm" | "dicom"))
}

fn format_label(path: &Path, vendor: Option<&str>, has_dicom_instances: bool) -> String {
    if has_dicom_instances || vendor == Some("dicom") {
        return "DICOM VL WSI".into();
    }

    match vendor {
        Some("aperio") => "Aperio WSI".into(),
        Some("generic-tiff") => "Generic TIFF WSI".into(),
        Some("hamamatsu" | "hamamatsu-ndpi") => "Hamamatsu WSI".into(),
        Some("leica") => "Leica WSI".into(),
        Some("mirax") => "MIRAX WSI".into(),
        Some("olympus") => "Olympus WSI".into(),
        Some("philips") => "Philips TIFF WSI".into(),
        Some("raw-jp2k") => "Raw JPEG 2000 WSI".into(),
        Some("svcache") => "wsi-rs cache WSI".into(),
        Some("zeiss") => "Zeiss WSI".into(),
        Some(other) if !other.is_empty() => format!("{other} WSI"),
        _ => format_label_from_extension(path).unwrap_or_else(|| "wsi-rs WSI".into()),
    }
}

fn format_label_from_extension(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let label = match extension.as_str() {
        "bif" => "Ventana/Roche TIFF WSI",
        "czi" | "zvi" => "Zeiss WSI",
        "dcm" | "dicom" => "DICOM VL WSI",
        "j2c" | "j2k" => "Raw JPEG 2000 WSI",
        "mrxs" => "MIRAX WSI",
        "ndpi" | "vms" | "vmu" => "Hamamatsu WSI",
        "scn" => "Leica WSI",
        "svcache" => "wsi-rs cache WSI",
        "svs" => "Aperio WSI",
        "tif" | "tiff" => "TIFF WSI",
        "vsi" => "Olympus WSI",
        _ => return None,
    };
    Some(label.into())
}

fn candidate_paths(path: &Path, source_kind: SourceKind) -> Result<Vec<PathBuf>> {
    match source_kind {
        SourceKind::File => Ok(vec![path.to_path_buf()]),
        SourceKind::Folder => {
            let mut paths = Vec::new();
            for entry in std::fs::read_dir(path).map_err(|source| ViewerError::Io {
                path: path.to_path_buf(),
                source,
            })? {
                let entry = entry.map_err(|source| ViewerError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                let entry_path = entry.path();
                if entry_path.is_file() {
                    paths.push(entry_path);
                }
            }
            paths.sort();
            Ok(paths)
        }
    }
}

fn open_metadata_object(path: &Path) -> Result<DefaultDicomObject> {
    OpenFileOptions::new()
        .read_until(tags::PIXEL_DATA)
        .open_file(path)
        .map_err(|source| ViewerError::DicomRead {
            path: path.to_path_buf(),
            source: Box::new(source),
        })
}

fn inspect_dicom_instance(path: &Path) -> Result<Option<(DicomInstanceSummary, Option<String>)>> {
    let obj = open_metadata_object(path)?;
    let sop_class_uid = obj.meta().media_storage_sop_class_uid().to_string();
    if sop_class_uid != uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE {
        return Ok(None);
    }

    let series_instance_uid = optional_string(&obj, tags::SERIES_INSTANCE_UID);
    Ok(Some((
        DicomInstanceSummary {
            path: path.to_path_buf(),
            sop_class_uid,
            series_instance_uid_present: series_instance_uid.is_some(),
            transfer_syntax_uid: obj.meta().transfer_syntax().to_string(),
            image_type: optional_string(&obj, tags::IMAGE_TYPE)
                .map(|raw| {
                    raw.split('\\')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            rows: optional_u32(&obj, tags::ROWS),
            columns: optional_u32(&obj, tags::COLUMNS),
            total_pixel_matrix_rows: optional_u32(&obj, tags::TOTAL_PIXEL_MATRIX_ROWS),
            total_pixel_matrix_columns: optional_u32(&obj, tags::TOTAL_PIXEL_MATRIX_COLUMNS),
            number_of_frames: optional_u32(&obj, tags::NUMBER_OF_FRAMES),
            pixel_spacing: optional_spacing(&obj),
            dimension_organization_type: optional_string(&obj, tags::DIMENSION_ORGANIZATION_TYPE),
            samples_per_pixel: optional_u32(&obj, tags::SAMPLES_PER_PIXEL),
            photometric_interpretation: optional_string(&obj, tags::PHOTOMETRIC_INTERPRETATION),
            planar_configuration: optional_u32(&obj, tags::PLANAR_CONFIGURATION),
            bits_allocated: optional_u32(&obj, tags::BITS_ALLOCATED),
            bits_stored: optional_u32(&obj, tags::BITS_STORED),
            high_bit: optional_u32(&obj, tags::HIGH_BIT),
            pixel_representation: optional_u32(&obj, tags::PIXEL_REPRESENTATION),
        },
        series_instance_uid,
    )))
}

fn optional_string(obj: &DefaultDicomObject, tag: Tag) -> Option<String> {
    obj.get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_end_matches('\0').trim().to_string())
        .filter(|value| !value.is_empty())
}

fn optional_u32(obj: &DefaultDicomObject, tag: Tag) -> Option<u32> {
    obj.get(tag)
        .and_then(|element| element.to_int::<u32>().ok())
}

fn optional_spacing(obj: &DefaultDicomObject) -> Option<(f64, f64)> {
    let spacing = obj
        .get(tags::PIXEL_SPACING)
        .and_then(|element| element.to_multi_float64().ok())?;
    if spacing.len() >= 2 {
        Some((spacing[1], spacing[0]))
    } else {
        None
    }
}

fn build_fact_warnings(instances: &[DicomInstanceSummary], levels: &[LevelInfo]) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut transfer_syntaxes = BTreeSet::new();
    let mut image_types = BTreeMap::<Vec<String>, usize>::new();
    let mut series_uid_presence = BTreeSet::new();

    for instance in instances {
        transfer_syntaxes.insert(instance.transfer_syntax_uid.clone());
        *image_types.entry(instance.image_type.clone()).or_insert(0) += 1;
        series_uid_presence.insert(instance.series_instance_uid_present);

        if let (Some(expected_frames), Some(frames)) = (
            expected_tiled_full_frame_count(instance),
            instance.number_of_frames,
        ) {
            if expected_frames != u64::from(frames) {
                warnings.push(format!(
                    "{} declares {frames} frame{} but a dense TILED_FULL grid expects {expected_frames}",
                    instance.path.file_name_display(),
                    if frames == 1 { "" } else { "s" }
                ));
            }
        }
    }

    if transfer_syntaxes.len() > 1 {
        warnings.push(format!(
            "series uses {} transfer syntaxes; verify this was intentional",
            transfer_syntaxes.len()
        ));
    }
    if image_types.len() > 3 {
        warnings.push(format!(
            "series contains {} distinct image type groups",
            image_types.len()
        ));
    }
    if series_uid_presence.contains(&false) {
        warnings.push("one or more DICOM instances are missing SeriesInstanceUID".into());
    }
    if levels.len() == 1 {
        warnings.push("only one pyramid level was detected".into());
    }
    for level in levels {
        if let LevelTileLayout::Regular {
            tile_width,
            tile_height,
            ..
        } = level.tile_layout
        {
            if tile_width == 0 || tile_height == 0 {
                warnings.push(format!("level {} has zero tile dimensions", level.index));
            }
        }
    }

    warnings
}

fn expected_tiled_full_frame_count(instance: &DicomInstanceSummary) -> Option<u64> {
    if instance.dimension_organization_type.as_deref() != Some("TILED_FULL") {
        return None;
    }
    let columns = instance.columns?;
    let rows = instance.rows?;
    if columns == 0 || rows == 0 {
        return None;
    }
    let matrix_columns = instance.total_pixel_matrix_columns?;
    let matrix_rows = instance.total_pixel_matrix_rows?;
    Some(u64::from(matrix_columns.div_ceil(columns)) * u64::from(matrix_rows.div_ceil(rows)))
}

trait PathDisplay {
    fn file_name_display(&self) -> String;
}

impl PathDisplay for Path {
    fn file_name_display(&self) -> String {
        self.file_name()
            .and_then(|name| name.to_str())
            .map_or_else(|| self.display().to_string(), str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dicom_core::value::PrimitiveValue;
    use dicom_core::{DataElement, VR};
    use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

    #[test]
    fn extracts_synthetic_wsi_facts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slide.dcm");
        write_test_dicom(
            &path,
            "1.2.826.0.1.3680043.10.777.1",
            "1.2.826.0.1.3680043.10.777",
        );

        let inspection = inspect_input(&path).unwrap();
        assert_eq!(inspection.instances.len(), 1);
        let instance = &inspection.instances[0];
        assert_eq!(instance.rows, Some(2));
        assert_eq!(instance.columns, Some(2));
        assert_eq!(instance.number_of_frames, Some(1));
        assert_eq!(
            instance.dimension_organization_type.as_deref(),
            Some("TILED_FULL")
        );
        assert_eq!(instance.samples_per_pixel, Some(3));
        assert_eq!(instance.photometric_interpretation.as_deref(), Some("RGB"));
        assert_eq!(instance.bits_stored, Some(8));
        assert_eq!(
            instance.transfer_syntax_uid,
            uids::EXPLICIT_VR_LITTLE_ENDIAN
        );
    }

    #[test]
    fn metadata_preflight_stops_before_pixel_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slide.dcm");
        write_test_dicom(
            &path,
            "1.2.826.0.1.3680043.10.777.1",
            "1.2.826.0.1.3680043.10.777",
        );

        let obj = open_metadata_object(&path).unwrap();
        assert!(obj.get(tags::PIXEL_DATA).is_none());
    }

    #[test]
    fn rejects_non_dicom_input_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-dicom.dcm");
        std::fs::write(&path, b"not dicom").unwrap();

        let err = ViewerStudy::open_path(&path).unwrap_err();
        assert!(
            matches!(err, ViewerError::Wsi(_)),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn opens_wsi_rs_raw_jp2k_without_dicom_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slide.j2k");
        std::fs::write(&path, j2k_test_support::htj2k_rgb8_fixture(32, 24)).unwrap();

        let study = ViewerStudy::open_path(&path).unwrap();
        let summary = study.summary();
        assert_eq!(summary.format_label, "Raw JPEG 2000 WSI");
        assert_eq!(summary.file_count, 1);
        assert_eq!(summary.dicom_instance_count, 0);
        assert_eq!(summary.levels.len(), 1);

        let tile = study
            .read_tile_rgba(LevelIndex::from_u32(0), TileCoord::new(0, 0))
            .unwrap();
        assert_eq!((tile.width, tile.height), (32, 24));
        assert_eq!(tile.rgba.len(), 32 * 24 * 4);
    }

    #[test]
    fn folder_scan_warns_about_mixed_series_candidates() {
        let dir = tempfile::tempdir().unwrap();
        write_test_dicom(
            &dir.path().join("a.dcm"),
            "1.2.826.0.1.3680043.10.777.1",
            "1.2.826.0.1.3680043.10.777",
        );
        write_test_dicom(
            &dir.path().join("b.dcm"),
            "1.2.826.0.1.3680043.10.778.1",
            "1.2.826.0.1.3680043.10.778",
        );

        let inspection = inspect_input(dir.path()).unwrap();
        assert_eq!(inspection.instances.len(), 2);
        let warnings = inspection.warnings;
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("distinct DICOM series")),
            "mixed folder scan should warn about series grouping: {warnings:?}"
        );
    }

    #[test]
    fn warns_when_tiled_full_frame_count_mismatches_dense_grid() {
        let instance = DicomInstanceSummary {
            path: PathBuf::from("bad.dcm"),
            sop_class_uid: uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.into(),
            series_instance_uid_present: true,
            transfer_syntax_uid: uids::EXPLICIT_VR_LITTLE_ENDIAN.into(),
            image_type: vec![
                "ORIGINAL".into(),
                "PRIMARY".into(),
                "VOLUME".into(),
                "NONE".into(),
            ],
            rows: Some(2),
            columns: Some(2),
            total_pixel_matrix_rows: Some(5),
            total_pixel_matrix_columns: Some(5),
            number_of_frames: Some(8),
            pixel_spacing: None,
            dimension_organization_type: Some("TILED_FULL".into()),
            samples_per_pixel: Some(3),
            photometric_interpretation: Some("RGB".into()),
            planar_configuration: Some(0),
            bits_allocated: Some(8),
            bits_stored: Some(8),
            high_bit: Some(7),
            pixel_representation: Some(0),
        };
        let warnings = build_fact_warnings(&[instance], &[]);

        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("dense TILED_FULL grid expects 9")),
            "expected dense-grid frame warning, got {warnings:?}"
        );
    }

    #[test]
    fn opens_and_reads_first_tile_through_wsi_rs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slide.dcm");
        write_test_dicom(
            &path,
            "1.2.826.0.1.3680043.10.777.1",
            "1.2.826.0.1.3680043.10.777",
        );

        let study = ViewerStudy::open_path(&path).unwrap();
        assert_eq!(study.summary().format_label, "DICOM VL WSI");
        assert_eq!(study.summary().file_count, 1);
        assert_eq!(study.summary().dicom_instance_count, 1);
        assert_eq!(study.summary().levels.len(), 1);
        let tile = study
            .read_tile_rgba(LevelIndex::from_u32(0), TileCoord::new(0, 0))
            .unwrap();
        assert_eq!((tile.width, tile.height), (2, 2));
        assert_eq!(tile.rgba.len(), 2 * 2 * 4);
        assert_eq!(&tile.rgba[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn whole_level_layout_uses_viewer_display_tiles() {
        let layout = LevelTileLayout::WholeLevel {
            width: 12960,
            height: 9472,
            virtual_tile_width: 480,
            virtual_tile_height: 8,
        };

        assert_eq!(
            layout.display_tile_size(),
            (DEFAULT_DISPLAY_TILE_SIZE, DEFAULT_DISPLAY_TILE_SIZE)
        );
        assert_eq!(layout.grid_size(), Some((26, 19)));
        assert!(layout.contains(TileCoord::new(25, 18)));
        assert!(!layout.contains(TileCoord::new(26, 18)));
    }

    #[test]
    #[ignore = "requires DICOM_VIEWER_WSI_FIXTURE to point at a local WSI file"]
    fn opens_local_wsi_fixture_from_env() {
        let path = std::env::var_os("DICOM_VIEWER_WSI_FIXTURE")
            .map(PathBuf::from)
            .expect("DICOM_VIEWER_WSI_FIXTURE must point at a local WSI file");
        let study = ViewerStudy::open_path(&path).unwrap();
        let summary = study.summary();
        assert!(!summary.levels.is_empty());
        let overview = summary
            .levels
            .iter()
            .min_by_key(|level| u128::from(level.width) * u128::from(level.height))
            .expect("summary has levels");
        let tile = study
            .read_tile_rgba(overview.index, TileCoord::new(0, 0))
            .unwrap();
        assert!(tile.width > 0);
        assert!(tile.height > 0);
        assert_eq!(
            tile.rgba.len(),
            tile.width as usize * tile.height as usize * 4
        );
    }

    fn write_test_dicom(path: &Path, sop_instance_uid: &'static str, series_uid: &'static str) {
        let mut object = InMemDicomObject::new_empty();
        object.put(DataElement::new(
            tags::SOP_CLASS_UID,
            VR::UI,
            uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE,
        ));
        object.put(DataElement::new(
            tags::SOP_INSTANCE_UID,
            VR::UI,
            sop_instance_uid,
        ));
        object.put(DataElement::new(
            tags::SERIES_INSTANCE_UID,
            VR::UI,
            series_uid,
        ));
        object.put(DataElement::new(
            tags::IMAGE_TYPE,
            VR::CS,
            "ORIGINAL\\PRIMARY\\VOLUME\\NONE",
        ));
        object.put(DataElement::new(
            tags::ROWS,
            VR::US,
            PrimitiveValue::from(2u16),
        ));
        object.put(DataElement::new(
            tags::COLUMNS,
            VR::US,
            PrimitiveValue::from(2u16),
        ));
        object.put(DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_ROWS,
            VR::UL,
            PrimitiveValue::from(2u32),
        ));
        object.put(DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_COLUMNS,
            VR::UL,
            PrimitiveValue::from(2u32),
        ));
        object.put(DataElement::new(
            tags::NUMBER_OF_FRAMES,
            VR::IS,
            PrimitiveValue::from(1u32),
        ));
        object.put(DataElement::new(
            tags::DIMENSION_ORGANIZATION_TYPE,
            VR::CS,
            "TILED_FULL",
        ));
        object.put(DataElement::new(
            tags::SAMPLES_PER_PIXEL,
            VR::US,
            PrimitiveValue::from(3u16),
        ));
        object.put(DataElement::new(
            tags::PHOTOMETRIC_INTERPRETATION,
            VR::CS,
            "RGB",
        ));
        object.put(DataElement::new(
            tags::PLANAR_CONFIGURATION,
            VR::US,
            PrimitiveValue::from(0u16),
        ));
        object.put(DataElement::new(
            tags::BITS_ALLOCATED,
            VR::US,
            PrimitiveValue::from(8u16),
        ));
        object.put(DataElement::new(
            tags::BITS_STORED,
            VR::US,
            PrimitiveValue::from(8u16),
        ));
        object.put(DataElement::new(
            tags::HIGH_BIT,
            VR::US,
            PrimitiveValue::from(7u16),
        ));
        object.put(DataElement::new(
            tags::PIXEL_REPRESENTATION,
            VR::US,
            PrimitiveValue::from(0u16),
        ));
        object.put(DataElement::new(
            tags::PIXEL_SPACING,
            VR::DS,
            "0.00025\\0.00025",
        ));
        object.put(DataElement::new(
            tags::PIXEL_DATA,
            VR::OB,
            PrimitiveValue::from(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]),
        ));
        object
            .with_meta(
                FileMetaTableBuilder::new()
                    .media_storage_sop_class_uid(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
                    .media_storage_sop_instance_uid(sop_instance_uid)
                    .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN),
            )
            .unwrap()
            .write_to_file(path)
            .unwrap();
    }
}
