# WSI Viewer

Lightweight desktop viewer for local whole-slide image files through `wsi-rs`.

The app is intended for checking `wsi-dicom` output locally and for verifying
other wsi-rs-supported WSI inputs. It does not upload files, use DICOMweb, or
expose patient-identifying tags by default.

## Build

This repo expects `../wsi-rs` and `../j2k` sibling checkouts when using
the local development path dependencies.

```sh
cargo run -p dicom-viewer
```

## Verify

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

Dual-licensed under either [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.

## Current Scope

- Desktop-only `egui/eframe` app.
- Open one wsi-rs-supported WSI file or a folder of DICOM instances.
- View WSI levels as tiled RGB/RGBA pixels through `wsi-rs`.
- Supported inputs follow wsi-rs's registered readers, including DICOM VL WSI,
  TIFF-family WSI, Zeiss CZI/ZVI, MIRAX, Hamamatsu VMS/VMU, Olympus VSI,
  wsi-rs `.svcache`, and raw JPEG 2000 codestreams.
- Show WSI facts: dimensions, tile grid, frame counts, transfer syntax,
  spacing, and non-PHI warnings when those facts are available.
