# WSI Viewer

Lightweight desktop viewer for local whole-slide image files through `wsi-rs`.

The app is intended for checking `wsi-dicom` output locally and for verifying
other wsi-rs-supported WSI inputs. It does not upload files or use DICOMweb.
Its facts panel reads only the technical WSI tags documented below; local file
paths can still be visible in the UI and in screenshots.

See [Architecture](docs/ARCHITECTURE.md) for ownership, scheduling, cache, and
Metal interoperability invariants.

## Build

This repo expects `../wsi-rs` version 0.5.2 and `../j2k` version 0.7.4 sibling
checkouts for its local path dependencies. CI pins `wsi-rs` to `v0.5.2` and
`j2k` to `v0.7.4`.

```sh
cargo run -p dicom-viewer
```

wgpu is the only presentation backend on every platform. On macOS, ordinary
builds automatically enable `wsi-rs` Metal decoding and use the renderer's
exact Metal device; resident RGB tiles are converted to RGBA by a wgpu compute
pass without host readback. CPU-decoded tiles use the same wgpu texture path.
On other platforms, the `cuda` feature lets `auto` reuse wsi-rs CUDA sessions
for compressed JPEG and JPEG 2000 decode. CUDA tiles cross the viewer boundary
only through checked, pitch-aware host download and then use the existing CPU
RGBA, ICC, cache, and wgpu upload path. wgpu remains the only renderer; there
is no CUDA-to-wgpu interop. A CUDA download failure receives exactly one
ordered CPU retry.

`DICOM_VIEWER_TILE_BACKEND` accepts `auto` (the default) or `cpu`. `auto`
prefers renderer-resident Metal tiles on macOS and falls back to CPU output for
unsupported codecs or failed imports. `cpu` forces CPU-resident decode output
while retaining wgpu presentation.

`DICOM_VIEWER_MEMORY_PROFILE` accepts `balanced` (the default: 256 MiB viewer,
128 MiB shared source, 32 MiB display) or `large` (512/256/64 MiB). Set
`DICOM_VIEWER_DEBUG_STATS=1` to show pipeline statistics in the canvas and
emit a schema-v4 JSONL diagnostic per second to stderr. Latency distributions
include sample counts and use `null` percentiles when no samples exist. The
`app_ui_cpu_ms` metric measures only the viewer's `eframe::App::ui` CPU work;
it does not include egui tessellation, GPU execution, or presentation. The
diagnostics use a trailing one-second window, report visible-lane queue latency
separately, and distinguish DICOM index work performed by level preparation
from indexing that raced inside a tile read. The `tile_probe` utility opens an
independent study for each trial, prepares the requested pyramid level, and
times identical first and warm batches through the selected production API:

```sh
cargo run --release -p dicom-viewer --bin tile_probe -- \
  --trials 5 --json --api controlled-render --backend auto --batch-size 8 sample.svs
```

Probe output calls the first batch “study-cold”; it does not claim to flush the
operating-system file cache. Use the live viewer JSONL for scheduler, source,
upload, and app-UI CPU diagnostics rather than treating `tile_probe` as an
end-to-end UI benchmark. End-to-end frame-time acceptance still requires an
external capture or additional renderer instrumentation.

For measurement only, `DICOM_VIEWER_TILE_WORKERS` and
`DICOM_VIEWER_JP2K_THREADS` override the bounded viewer-worker and JP2K CPU
thread counts. `DICOM_VIEWER_INTERACTIVE_BATCH_SIZE` accepts `2`, `4`, or `8`
for CPU-backed interactive tile reads and defaults to `2`. Invalid values
retain the measured defaults.

The viewer-memory ceiling covers store-owned decoded tiles, ready textures,
and the source-plus-destination overlap of synchronous upload. It does not
cover decoder scratch space, wsi-rs source caches, channel messages, or GPU
driver overhead.

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
