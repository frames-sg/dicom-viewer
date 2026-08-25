# WSI Viewer

Lightweight desktop viewer for local whole-slide image files through `wsi-rs`.

This application is for research use only. It is not a medical device and is
not intended for diagnosis, treatment decisions, or other clinical use. Use
only research inputs that contain no patient data; the viewer does not perform
de-identification or validate that an input is free of identifying metadata.

The app is intended for checking `wsi-dicom` output locally and for verifying
other wsi-rs-supported WSI inputs. It does not upload files or use DICOMweb.
Its facts panel reads only the technical WSI tags documented below; local file
paths can still be visible in the UI and in screenshots.

See [Architecture](docs/ARCHITECTURE.md) for ownership, scheduling, cache, and
Metal interoperability invariants, the [pathology annotation
workflow](docs/ANNOTATION_WORKFLOW.md), and the [research release
checklist](docs/RELEASE.md) for distribution gates.

## Build

The viewer resolves `wsi-rs` 0.6.0 at revision `b940ea94` and J2K 0.10.0 at
revision `57b6af89` from their upstream Git repositories. `Cargo.lock` pins the
complete revisions, so no sibling codec checkout is required.

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
the source-plus-destination overlap of synchronous upload, and a shared
reservation for concurrent decoded batches. It does not cover decoder scratch
space, wsi-rs source caches, or GPU driver overhead.

DICOM inspection rejects metadata beyond explicit resource limits before the
eager object parser runs: 1 MiB of file-meta data, 16 MiB per primitive value,
128 MiB of cumulative primitive values, two million metadata tokens, and 64
nested sequences. These are research-viewer safety limits rather than DICOM
conformance claims.

## Pathology annotation workspace

The desktop frontend is native `eframe`/`egui`, with egui/epaint rendered by
wgpu. It does not use Svelte or Tauri. The viewer's tile renderer, camera, and
annotation overlays share that one native state model and GPU device.

With a slide open, use the single **Pathology** workspace. Its left rail has
Pan, Select, Polygon, Brush, Point, and Ruler; its right panel has the pinned
controlled Annotation Scheme, virtualized Findings, Layers, the selection
inspector, and expert Terminology & DICOM controls.

- Vector Polygon and Point create independent tracked findings. Same-class
  regions never merge globally.
- Segmentation Polygon and Brush modify one independently tracked segment with
  Add/Erase primitives. `N` starts the next segment even when its class is the
  same. Erase is not a terminology class.
- Ruler creates a tracked length and exports through the fixed pathology SR
  report when reliable DICOM frame identity and physical spacing are present.
- Every finding, segment, and ruler keeps a UUID, non-reused ordinal, Tracking
  ID, and Tracking UID across exports.

Imported ANN, SEG, SR, profiled GeoJSON, masks, and heatmaps start as locked
source layers. Unknown concepts remain isolated. A source object becomes a
tracked finding only after explicit class mapping and **Promote**, or the whole
lossless layer becomes editable only after every class is mapped. Imports
never create palette classes or mutate the scheme library.

**Import** and **Export** replace the old fragmented annotation and Derived
controls. Export preflight covers portable workspace, scheme-aware GeoJSON,
CellViT compatibility GeoJSON, ANN, SEG, SR, and PM. Incompatible content is
listed and requires an explicit **Export eligible items only** decision; vector
rasterization into SEG is separately opt-in. Jobs are cancellable and publish
through temporary destinations, never overwrite the open source, and require
an explicit decision for existing output.

Autosave uses revisioned, source-identity-addressed snapshots and restores an
unfinished polygon without treating it as committed geometry. The viewer
retains five valid revisions per source; **Start fresh** archives prior work.

See the [pathology workflow](docs/ANNOTATION_WORKFLOW.md), [Annotation Scheme
contract](docs/ANNOTATION_SCHEME_V1.md), [scheme-aware GeoJSON
contract](docs/FRAMES_PATHOLOGY_GEOJSON_V1.md), [tumor-mask compatibility
adapter](docs/TUMOR_MASK_COMPATIBILITY.md), and [workspace storage/privacy
notes](docs/WORKSPACE_STORAGE.md).

### Headless annotation interoperability probe

`annotation_probe` is a thin CLI over the separately versioned
`wsi-dicom-annotations` library. It exposes ANN/SEG parsing and rewriting plus
Rust-owned GeoJSON and raster conversion without the GUI. It writes one
schema-versioned JSON object to stdout; warnings and human-readable failures go
to stderr.

```text
annotation_probe inspect --source source.dcm [--canonical-source level0.dcm] [--payload full|digest] annotations.dcm
annotation_probe roundtrip --source source.dcm [--canonical-source level0.dcm] --output rewritten.dcm [--allow-lossy] [--payload full|digest] annotations.dcm
annotation_probe convert-geojson --source source.dcm --mapping mapping.json --coordinate-space level0-pixels --target ann --output ann.dcm annotations.geojson
annotation_probe convert-raster --source source.dcm --profile raster-profile.json --output map.dcm probabilities.npy
```

The input SOP Class selects ANN or SEG automatically. ANN supports 2D `VOLUME`, 2D `FRAME`,
3D common-Z, and 3D XYZ data for all five standard graphic types. SEG inspection supports
binary, label-map, and fractional objects; rewriting is deliberately limited to binary SEG.
By default, any known semantic loss rejects a rewrite before an output is created.
`--allow-lossy` is the explicit expert override.

`full` payloads contain normalized coordinates or row runs. `digest` payloads contain stable
SHA-256 summaries and are intended for large-scale studies. Runtime timing and tracked-heap
measurements are kept outside the deterministic `semantic` object.

The conversion commands write ANN, SEG, Comprehensive 3D SR, and Parametric
Map derived objects with explicit profiles, provenance, bounded I/O, and
atomic publication. See [DICOM-native pathology conversion](docs/DICOM_NATIVE_CONVERSION.md)
for the complete command and data contracts.

### Test DICOM-derived data in the viewer

Open a VL WSI DICOM instance directly, then select **Import** in the Pathology
workspace:

```sh
cargo run -p dicom-viewer -- /path/to/source-wsi.dcm
```

- **Profiled GeoJSON:** choose **Profiled GeoJSON + class mapping…**, select the
  GeoJSON and mapping profile, then inspect the locked source layer. Map source
  classes before promoting one object or converting the complete lossless
  layer.
- **Structured Report:** choose **DICOM SR…** for direct report geometry, or
  **DICOM SR with companion SEG…** and explicitly select the referenced SEG.
  Compatible two-point lengths can be promoted after mapping; other report
  content remains locked.
- **Raster mask or heatmap:** select the raster/profile source, inspect the
  bounded overlay, then use **Existing heatmap source (PM)…** to stream a new
  Parametric Map bundle. DICOM PM import is not claimed because there is no PM
  reader.

The GUI performs the same bounded semantic preflight and verified writes as
`annotation_probe`. Existing destinations require an explicit replace choice;
failed or cancelled publication leaves them unchanged. The probe remains
useful for repeatable headless tests and independent interoperability harnesses.

## Verify

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo check --workspace --all-targets --locked --features cuda
cargo build --workspace --release --locked
cargo metadata --locked --format-version 1
cargo machete
cargo audit --deny unsound
cargo deny check advisories bans licenses sources
```

## License

Dual-licensed under either [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.

## Current Scope

- Research-use-only operation with non-patient inputs; no clinical claims or
  de-identification workflow.
- Desktop-only `egui/eframe` app with a unified wgpu renderer.
- Open one wsi-rs-supported WSI file or a folder of DICOM instances.
- View WSI levels as tiled RGB/RGBA pixels through `wsi-rs`.
- Create independently tracked vector findings, segmentation segments, and
  rulers under one pinned versioned Annotation Scheme.
- Import ANN/SEG/SR and profiled results as isolated source layers; explicitly
  map and promote lossless objects without creating workspace-local classes.
- Export portable workspace, scheme-aware or exact CellViT GeoJSON, ANN, SEG,
  fixed-semantics measurement SR, and existing TIFF/NPY/Zarr/tiled heatmap
  sources as DICOM Parametric Maps through Pathology or the headless probe.
- Supported inputs follow wsi-rs's registered readers, including DICOM VL WSI,
  TIFF-family WSI, Zeiss CZI/ZVI, MIRAX, Hamamatsu VMS/VMU, Olympus VSI,
  wsi-rs `.svcache`, and raw JPEG 2000 codestreams.
- Show WSI facts: dimensions, tile grid, frame counts, transfer syntax,
  spacing, and non-PHI warnings when those facts are available.
