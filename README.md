# WSI Viewer

Lightweight desktop viewer for local whole-slide image files through `wsi-rs`.

The app is intended for checking `wsi-dicom` output locally and for verifying
other wsi-rs-supported WSI inputs. It does not upload files or use DICOMweb.
Its facts panel reads only the technical WSI tags documented below; local file
paths can still be visible in the UI and in screenshots.

## Build

This repo expects `../wsi-rs` version 0.5.0 and `../j2k` version 0.7.4 sibling
checkouts for its local path dependencies. CI pins `wsi-rs` to `v0.5.0` and
`j2k` to `v0.7.4`.

```sh
cargo run -p dicom-viewer
```

wgpu is the only presentation backend on every platform. On macOS, ordinary
builds automatically enable `wsi-rs` Metal decoding and use the renderer's
exact Metal device; resident RGB tiles are converted to RGBA by a wgpu compute
pass without host readback. CPU-decoded tiles use the same wgpu texture path.
The `cuda` feature enables the corresponding decode capability but does not
create another renderer.

`DICOM_VIEWER_TILE_BACKEND` accepts `auto` (the default) or `cpu`. `auto`
prefers renderer-resident Metal tiles on macOS and falls back to CPU output for
unsupported codecs or failed imports. `cpu` forces CPU-resident decode output
while retaining wgpu presentation.

## Verify

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo check --workspace --all-targets --locked --features cuda
cargo build --workspace --release --locked
cargo machete
cargo audit --deny unsound
cargo deny check advisories bans licenses sources
```

## License

Dual-licensed under either [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.

## Current Scope

- Desktop-only `egui/eframe` app with a unified wgpu renderer.
- Open one wsi-rs-supported WSI file or a folder of DICOM instances.
- View WSI levels as tiled RGB/RGBA pixels through `wsi-rs`.
- Supported inputs follow wsi-rs's registered readers, including DICOM VL WSI,
  TIFF-family WSI, Zeiss CZI/ZVI, MIRAX, Hamamatsu VMS/VMU, Olympus VSI,
  wsi-rs `.svcache`, and raw JPEG 2000 codestreams.
- Show WSI facts: dimensions, tile grid, frame counts, transfer syntax,
  spacing, and non-PHI warnings when those facts are available.
