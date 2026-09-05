#![allow(unsafe_code)]

use std::{
    cell::RefCell,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use dwrote::{
    DWRITE_FONT_SIMULATIONS_NONE, DWRITE_GLYPH_RUN, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC, DWRITE_TEXTURE_ALIASED_1x1, FontFile,
    GlyphRunAnalysis,
};
use winapi::{
    Interface as _,
    shared::winerror::E_POINTER,
    um::{
        dwrite::{
            DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory,
            IDWriteGlyphRunAnalysis,
        },
        dwrite_1::DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
        dwrite_2::{DWRITE_GRID_FIT_MODE_ENABLED, IDWriteFactory2},
        unknwnbase::IUnknown,
    },
};
use wio::com::ComPtr;

use super::fonts::Blob;

const MAX_CACHED_FONT_FACES: usize = 32;
const MAX_GLYPH_SIDE: i32 = 2048;

static NEXT_FONT_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static FONT_FACES: RefCell<Vec<CachedFontFace>> = const { RefCell::new(Vec::new()) };
    static DIRECTWRITE_FACTORY: Result<ComPtr<IDWriteFactory2>, dwrote::HRESULT> = create_directwrite_factory();
}

struct CachedFontFace {
    id: u64,
    face: dwrote::FontFace,
}

pub(super) struct DirectWriteFont {
    id: u64,
    bytes: Blob,
    face_index: u32,
    failure_reported: AtomicBool,
}

impl DirectWriteFont {
    pub(super) fn new(bytes: Blob, face_index: u32) -> Self {
        Self {
            id: NEXT_FONT_ID.fetch_add(1, Ordering::Relaxed),
            bytes,
            face_index,
            failure_reported: AtomicBool::new(false),
        }
    }

    pub(super) fn rasterize(
        &self,
        glyph_id: skrifa::GlyphId,
        em_size_pixels: f32,
        baseline_x_pixels: f32,
    ) -> Option<GlyphBitmap> {
        match self.try_rasterize(glyph_id, em_size_pixels, baseline_x_pixels) {
            Ok(bitmap) => Some(bitmap),
            Err(error) => {
                if !self.failure_reported.swap(true, Ordering::Relaxed) {
                    log::warn!(
                        "DirectWrite glyph rasterization failed for this font; falling back to the portable rasterizer: {error}"
                    );
                }
                None
            }
        }
    }

    fn try_rasterize(
        &self,
        glyph_id: skrifa::GlyphId,
        em_size_pixels: f32,
        baseline_x_pixels: f32,
    ) -> Result<GlyphBitmap, RasterizationError> {
        if !(em_size_pixels.is_finite() && em_size_pixels > 0.0) {
            return Err(RasterizationError::InvalidEmSize(em_size_pixels));
        }
        let glyph_index = glyph_id.to_u32();
        if glyph_index > u32::from(u16::MAX) {
            return Err(RasterizationError::GlyphIndex(glyph_index));
        }
        let glyph_index = glyph_index as u16;

        with_font_face(self, |face| {
            let glyph_run = DWRITE_GLYPH_RUN {
                // SAFETY: `face` owns a valid DirectWrite COM font-face pointer and
                // remains alive until the glyph-run analysis has been created.
                fontFace: unsafe { face.as_ptr() },
                fontEmSize: em_size_pixels,
                glyphCount: 1,
                glyphIndices: &glyph_index,
                glyphAdvances: std::ptr::null(),
                glyphOffsets: std::ptr::null(),
                isSideways: 0,
                bidiLevel: 0,
            };
            let analysis = create_grayscale_analysis(&glyph_run, baseline_x_pixels)?;
            let bounds = analysis
                .get_alpha_texture_bounds(DWRITE_TEXTURE_ALIASED_1x1)
                .map_err(|code| RasterizationError::DirectWrite("GetAlphaTextureBounds", code))?;
            let width = bounds
                .right
                .checked_sub(bounds.left)
                .ok_or(RasterizationError::InvalidBounds)?;
            let height = bounds
                .bottom
                .checked_sub(bounds.top)
                .ok_or(RasterizationError::InvalidBounds)?;
            if width < 0
                || height < 0
                || width > MAX_GLYPH_SIDE
                || height > MAX_GLYPH_SIDE
                || width.checked_mul(height).is_none()
            {
                return Err(RasterizationError::InvalidBounds);
            }
            if width == 0 || height == 0 {
                return Ok(GlyphBitmap {
                    left: bounds.left,
                    top: bounds.top,
                    width: 0,
                    height: 0,
                    coverage: Vec::new(),
                });
            }

            let coverage = analysis
                .create_alpha_texture(DWRITE_TEXTURE_ALIASED_1x1, bounds)
                .map_err(|code| RasterizationError::DirectWrite("CreateAlphaTexture", code))?;
            let width = width as usize;
            let height = height as usize;
            let expected_len = width
                .checked_mul(height)
                .ok_or(RasterizationError::InvalidBounds)?;
            if coverage.len() != expected_len {
                return Err(RasterizationError::UnexpectedCoverage {
                    expected: expected_len,
                    actual: coverage.len(),
                });
            }

            Ok(GlyphBitmap {
                left: bounds.left,
                top: bounds.top,
                width,
                height,
                coverage,
            })
        })
    }
}

fn create_directwrite_factory() -> Result<ComPtr<IDWriteFactory2>, dwrote::HRESULT> {
    let mut unknown = std::ptr::null_mut::<IUnknown>();
    // SAFETY: DirectWrite receives the IID for `IDWriteFactory` and a valid,
    // writable out-pointer. A successful call owns one COM reference.
    let result = unsafe {
        DWriteCreateFactory(
            DWRITE_FACTORY_TYPE_SHARED,
            &IDWriteFactory::uuidof(),
            &mut unknown,
        )
    };
    if result < 0 {
        return Err(result);
    }
    if unknown.is_null() {
        return Err(E_POINTER);
    }
    // SAFETY: the successful factory call returned an owned `IDWriteFactory`
    // COM pointer through `unknown`; `ComPtr` assumes that reference exactly once.
    let factory = unsafe { ComPtr::<IDWriteFactory>::from_raw(unknown.cast()) };
    factory.cast::<IDWriteFactory2>()
}

fn create_grayscale_analysis(
    glyph_run: &DWRITE_GLYPH_RUN,
    baseline_x_pixels: f32,
) -> Result<GlyphRunAnalysis, RasterizationError> {
    DIRECTWRITE_FACTORY.with(|factory| {
        let factory = factory.as_ref().map_err(|code| {
            RasterizationError::DirectWrite("QueryInterface(IDWriteFactory2)", *code)
        })?;
        let mut analysis = std::ptr::null_mut::<IDWriteGlyphRunAnalysis>();
        // SAFETY: `factory` is an owned DirectWrite factory; `glyph_run` points
        // to one live glyph index and a live font face for this call; the optional
        // arrays and transform are null as permitted by DirectWrite; `analysis`
        // is a valid writable out-pointer.
        let result = unsafe {
            factory.CreateGlyphRunAnalysis(
                glyph_run,
                std::ptr::null(),
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_ENABLED,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                baseline_x_pixels,
                0.0,
                &mut analysis,
            )
        };
        if result < 0 {
            return Err(RasterizationError::DirectWrite(
                "IDWriteFactory2::CreateGlyphRunAnalysis",
                result,
            ));
        }
        if analysis.is_null() {
            return Err(RasterizationError::DirectWrite(
                "IDWriteFactory2::CreateGlyphRunAnalysis",
                E_POINTER,
            ));
        }
        // SAFETY: the successful DirectWrite call returned one owned
        // `IDWriteGlyphRunAnalysis` COM reference through `analysis`.
        let analysis = unsafe { ComPtr::from_raw(analysis) };
        Ok(GlyphRunAnalysis::take(analysis))
    })
}

pub(super) struct GlyphBitmap {
    pub(super) left: i32,
    pub(super) top: i32,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) coverage: Vec<u8>,
}

fn with_font_face<T>(
    font: &DirectWriteFont,
    use_face: impl FnOnce(&dwrote::FontFace) -> Result<T, RasterizationError>,
) -> Result<T, RasterizationError> {
    FONT_FACES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let position = cache.iter().position(|entry| entry.id == font.id);
        let position = if let Some(position) = position {
            position
        } else {
            let file = FontFile::new_from_buffer(Arc::clone(&font.bytes))
                .ok_or(RasterizationError::FontFile)?;
            let face = file
                .create_face(font.face_index, DWRITE_FONT_SIMULATIONS_NONE)
                .map_err(|code| RasterizationError::DirectWrite("CreateFontFace", code))?;
            if cache.len() == MAX_CACHED_FONT_FACES {
                cache.remove(0);
            }
            cache.push(CachedFontFace { id: font.id, face });
            cache.len() - 1
        };
        use_face(&cache[position].face)
    })
}

#[derive(Debug)]
enum RasterizationError {
    DirectWrite(&'static str, dwrote::HRESULT),
    FontFile,
    GlyphIndex(u32),
    InvalidBounds,
    InvalidEmSize(f32),
    UnexpectedCoverage { expected: usize, actual: usize },
}

impl fmt::Display for RasterizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DirectWrite(operation, code) => {
                write!(
                    formatter,
                    "{operation} returned HRESULT 0x{:08X}",
                    *code as u32
                )
            }
            Self::FontFile => formatter.write_str("DirectWrite rejected the in-memory font"),
            Self::GlyphIndex(index) => write!(formatter, "glyph index {index} exceeds u16"),
            Self::InvalidBounds => formatter.write_str("DirectWrite returned invalid glyph bounds"),
            Self::InvalidEmSize(size) => write!(formatter, "invalid glyph em size {size}"),
            Self::UnexpectedCoverage { expected, actual } => write!(
                formatter,
                "DirectWrite returned {actual} coverage bytes; expected {expected}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use skrifa::MetadataProvider as _;

    use super::*;

    #[test]
    fn segoe_ui_produces_bounded_grayscale_coverage() {
        let windows_directory = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        let bytes = std::fs::read(
            PathBuf::from(windows_directory)
                .join("Fonts")
                .join("segoeui.ttf"),
        )
        .expect("Windows must provide Segoe UI");
        let font_ref = skrifa::FontRef::new(&bytes).expect("Segoe UI must be a valid font");
        let glyph_id = font_ref
            .charmap()
            .map('A')
            .expect("Segoe UI must contain Latin capital A");
        let font = DirectWriteFont::new(Arc::new(bytes), 0);

        let bitmap = font
            .try_rasterize(glyph_id, 20.25, 0.25)
            .expect("DirectWrite must rasterize Segoe UI");

        assert!(bitmap.width > 0);
        assert!(bitmap.height > 0);
        assert_eq!(bitmap.coverage.len(), bitmap.width * bitmap.height);
        assert!(bitmap.coverage.iter().any(|alpha| *alpha > 0));
        assert!(
            bitmap
                .coverage
                .iter()
                .any(|alpha| (1..u8::MAX).contains(alpha))
        );
    }
}
