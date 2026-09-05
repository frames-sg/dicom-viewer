//! Everything related to text, fonts, text layout, cursors etc.

pub mod cursor;
mod font;
mod fonts;
mod text_layout;
mod text_layout_types;
#[cfg(target_os = "windows")]
mod windows_directwrite;

/// One `\t` character is this many spaces wide.
pub const TAB_SIZE: usize = 4;

pub use {
    fonts::{
        FontData, FontDefinitions, FontFamily, FontId, FontInsert, FontPriority, FontTweak, Fonts,
        FontsImpl, FontsView, InsertFontFamily,
    },
    text_layout::*,
    text_layout_types::*,
};

/// Name of the platform glyph rasterizer used by this build.
///
/// This is exposed for downstream packaging tests. Text layout and painting
/// remain owned by `epaint` on every platform.
#[doc(hidden)]
pub const fn font_rasterizer_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "DirectWrite grayscale"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "skrifa/vello"
    }
}

/// Suggested character to use to replace those in password text fields.
pub const PASSWORD_REPLACEMENT_CHAR: char = '•';

/// Controls how we render text
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct TextOptions {
    /// Maximum size of the font texture.
    pub max_texture_side: usize,

    /// Controls how to convert glyph coverage to alpha.
    pub alpha_from_coverage: crate::AlphaFromCoverage,

    /// Whether to enable font hinting
    ///
    /// (round some font coordinates to pixels for sharper text).
    ///
    /// Default is `true`.
    pub font_hinting: bool,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            max_texture_side: 2048, // Small but portable
            alpha_from_coverage: crate::AlphaFromCoverage::default(),
            font_hinting: true,
        }
    }
}
