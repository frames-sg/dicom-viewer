# Local epaint patch

This directory is the crates.io source for `epaint` 0.34.3, pinned because the
public API does not provide a glyph-rasterizer hook.

The local change is intentionally limited to text rasterization on Windows:

- `src/text/windows_directwrite.rs` rasterizes individual glyphs with
  DirectWrite's grayscale alpha texture API and keeps a bounded thread-local
  font-face cache.
- `src/text/font.rs` places those alpha masks in the existing epaint atlas,
  preserving epaint's layout, clipping, caching, and wgpu rendering. A failed
  DirectWrite call is reported once per font and falls back to the unchanged
  portable rasterizer.
- `src/text/mod.rs` exposes the active rasterizer name for the viewer's
  packaging regression test.

Non-Windows builds compile the original skrifa/vello path. When updating egui,
compare these three files with the matching upstream `epaint` release before
moving or dropping the patch.

Upstream license: MIT OR Apache-2.0. The Windows-only `dwrote` dependency is
MPL-2.0 and is used unmodified.
