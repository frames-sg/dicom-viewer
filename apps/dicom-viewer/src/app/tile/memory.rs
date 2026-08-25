use dicom_viewer_core::{LevelInfo, TileCoord};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(in crate::app) enum TileFootprintError {
    #[error("tile dimensions {width}x{height} must be nonzero")]
    ZeroDimensions { width: u32, height: u32 },
    #[error("tile column {coordinate} overflows level {level} pixel coordinates")]
    ColumnCoordinateOverflow { coordinate: u64, level: u32 },
    #[error("tile row {coordinate} overflows level {level} pixel coordinates")]
    RowCoordinateOverflow { coordinate: u64, level: u32 },
    #[error("tile {col},{row} is outside level {level} dimensions {width}x{height}")]
    OutsideLevel {
        col: u64,
        row: u64,
        level: u32,
        width: u64,
        height: u64,
    },
    #[error("tile dimensions {width}x{height} overflow final RGBA texture bytes")]
    TextureByteOverflow { width: u32, height: u32 },
    #[error("tile dimensions {width}x{height} overflow upload peak bytes")]
    UploadPeakOverflow { width: u32, height: u32 },
    #[error(
        "decoded CPU tile dimensions {width}x{height} require {required} RGBA bytes, got {actual}"
    )]
    CpuByteLength {
        width: u32,
        height: u32,
        required: usize,
        actual: usize,
    },
}

/// Checked memory requirements for one actual tile.
///
/// `cpu_rgba_bytes` describes the CPU RGBA allocation when that allocation is the
/// decoded source; it is not added to the peak a second time. Temporary conversion
/// storage is separate and is zero for the current CPU-RGBA and Metal-to-texture paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct TileFootprint {
    width: u32,
    height: u32,
    decoded_source_bytes: usize,
    cpu_rgba_bytes: usize,
    final_texture_bytes: usize,
    peak_upload_bytes: usize,
}

impl TileFootprint {
    pub(in crate::app) fn for_level_tile(
        level: &LevelInfo,
        coord: TileCoord,
    ) -> Result<Self, TileFootprintError> {
        let (tile_width, tile_height) = level.tile_layout.display_tile_size();
        let x = coord.col().checked_mul(u64::from(tile_width)).ok_or(
            TileFootprintError::ColumnCoordinateOverflow {
                coordinate: coord.col(),
                level: level.index.get(),
            },
        )?;
        let y = coord.row().checked_mul(u64::from(tile_height)).ok_or(
            TileFootprintError::RowCoordinateOverflow {
                coordinate: coord.row(),
                level: level.index.get(),
            },
        )?;
        if x >= level.width || y >= level.height {
            return Err(TileFootprintError::OutsideLevel {
                col: coord.col(),
                row: coord.row(),
                level: level.index.get(),
                width: level.width,
                height: level.height,
            });
        }
        let width = u32::try_from(level.width.saturating_sub(x).min(u64::from(tile_width)))
            .map_err(|_| TileFootprintError::TextureByteOverflow {
                width: tile_width,
                height: tile_height,
            })?;
        let height = u32::try_from(level.height.saturating_sub(y).min(u64::from(tile_height)))
            .map_err(|_| TileFootprintError::TextureByteOverflow {
                width: tile_width,
                height: tile_height,
            })?;
        Self::for_rgba_dimensions(width, height)
    }

    pub(in crate::app) fn for_rgba_dimensions(
        width: u32,
        height: u32,
    ) -> Result<Self, TileFootprintError> {
        let rgba_bytes = Self::rgba_texture_bytes(width, height)?;
        Self::from_components(width, height, rgba_bytes, rgba_bytes)
    }

    pub(in crate::app) fn for_cpu_rgba(
        width: u32,
        height: u32,
        actual_bytes: usize,
    ) -> Result<Self, TileFootprintError> {
        let required = Self::rgba_texture_bytes(width, height)?;
        if actual_bytes != required {
            return Err(TileFootprintError::CpuByteLength {
                width,
                height,
                required,
                actual: actual_bytes,
            });
        }
        Self::from_components(width, height, actual_bytes, required)
    }

    #[cfg(target_os = "macos")]
    pub(in crate::app) fn for_device_decoded(
        width: u32,
        height: u32,
        decoded_source_bytes: usize,
    ) -> Result<Self, TileFootprintError> {
        Self::from_components(width, height, decoded_source_bytes, 0)
    }

    fn from_components(
        width: u32,
        height: u32,
        decoded_source_bytes: usize,
        cpu_rgba_bytes: usize,
    ) -> Result<Self, TileFootprintError> {
        let final_texture_bytes = Self::rgba_texture_bytes(width, height)?;
        let peak_upload_bytes = decoded_source_bytes
            .checked_add(final_texture_bytes)
            .ok_or(TileFootprintError::UploadPeakOverflow { width, height })?;
        Ok(Self {
            width,
            height,
            decoded_source_bytes,
            cpu_rgba_bytes,
            final_texture_bytes,
            peak_upload_bytes,
        })
    }

    pub(in crate::app) fn rgba_texture_bytes(
        width: u32,
        height: u32,
    ) -> Result<usize, TileFootprintError> {
        if width == 0 || height == 0 {
            return Err(TileFootprintError::ZeroDimensions { width, height });
        }
        usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(TileFootprintError::TextureByteOverflow { width, height })
    }

    pub(in crate::app) const fn dimensions(self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(in crate::app) const fn decoded_source_bytes(self) -> usize {
        self.decoded_source_bytes
    }

    pub(in crate::app) const fn cpu_rgba_bytes(self) -> usize {
        self.cpu_rgba_bytes
    }

    pub(in crate::app) const fn final_texture_bytes(self) -> usize {
        self.final_texture_bytes
    }

    pub(in crate::app) const fn temporary_conversion_bytes(self) -> usize {
        0
    }

    pub(in crate::app) const fn peak_upload_bytes(self) -> usize {
        self.peak_upload_bytes
    }

    pub(in crate::app) const fn in_flight_reservation_bytes(self) -> usize {
        self.peak_upload_bytes
    }
}
