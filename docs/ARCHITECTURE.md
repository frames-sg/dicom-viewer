# Architecture

This document describes the boundaries and invariants of the desktop viewer.
It is a map of the current implementation, not a roadmap.

## Workspace boundaries

| Area | Responsibility |
| --- | --- |
| `apps/dicom-viewer` | eframe/egui application and Pathology interaction state, revision storage, demand planning, worker ownership, tile cache, upload, and epaint/wgpu presentation |
| `crates/dicom-viewer-core` | wsi-rs façade, input inspection, canonical slide geometry, durable pathology workspace and export adapters, renderer-facing tile conversion, and color-management policy |
| `wsi-dicom-annotations` | UI-independent ANN/SEG/SR/PM models, readers, writers, coded terminology, profiled GeoJSON/raster conversion, publication, and semantic verification |
| `crates/metal-wgpu-interop` | Audited macOS-only ownership boundary for importing immutable J2K Metal allocations into the renderer's wgpu device |
| `wsi-rs` | Format detection, metadata, source I/O, tile extraction, codec dispatch, and source/display caches |

The app crate is the composition root. Core does not own UI state, and the
interop crate is the only viewer crate allowed to perform raw wgpu-hal or
Objective-C operations. The frontend is native `eframe`/`egui`; epaint feeds
the existing wgpu renderer. There is no WebView, Svelte, or Tauri boundary.

## Pathology workspace and DICOM sidecars

The VL WSI instance remains the immutable, tile-streamed image source. The app
has one native **Pathology** workspace, not independent annotation,
measurement, mask, report, and Derived documents. `WorkspaceDocument` is the
only serializable source of truth and owns:

- the source identity and embedded pinned Annotation Scheme snapshot;
- editable vector layers and independently tracked point/region findings;
- editable segmentation layers and independently tracked Add/Erase segments;
- tracked linear measurements;
- external-layer references and explicit semantic mappings;
- restorable presentation state.

`WorkspaceRuntime` owns nonserialized interaction and resource state: one
`ActiveTool`, selection/drafts/pointer capture, undo/redo, loaded external
payloads, R-tree/render caches, and background jobs. GPU textures and meshes
remain owned by the UI thread. An unfinished polygon has a small recoverable
`DraftInteraction` in revision snapshots but is not committed geometry.

Vector findings and segmentation segments are deliberately different data
representations behind one UI. Vectors contain points or independent simple
polygon components and cannot encode boolean holes. Segments retain ordered
polygon/brush Add/Erase primitives and compose per tracked segment through
`geo`; objects of one class never merge globally. Immutable coordinate slices
and document snapshots make command history and autosave cheap. History is
bounded to 500 commands or 256 MiB and is not serialized.

Same-directory discovery stops before large ANN coordinates, SEG per-frame
groups/Pixel Data, or SR content. The user-triggered background loader creates
a locked external-layer payload. Imported concepts remain isolated until every
source class is explicitly mapped to a geometry-compatible controlled class;
one object can then be promoted, or a complete layer converted atomically.
Fractional masks and unsupported geometry remain read-only.

`wsi-dicom-annotations` owns ANN/SEG/SR/PM construction and parsing, coded
terminology, tracking primitives, profiled GeoJSON/raster conversion, and
derived-object publication. Viewer core owns `WorkspaceDocument`, scheme-aware
GeoJSON, the named tumor-mask compatibility adapter, and the explicit adapters
and preflight decisions that translate workspace objects into library document
types. ANN accepts only directly representable vector points/simple polygons.
SEG accepts segments and explicitly requested vector rasterization. SR uses the
fixed v1 pathology linear-measurement report. PM streams only from an existing
profiled heatmap source; the app does not claim PM import.

All file exports run as cancellable jobs through unique temporary destinations.
The opened source is protected, existing output requires an explicit replace
decision, and cancellation/failure leaves prior output unchanged. New derived
SOP Instance UIDs are created per export while stored tracking identities are
reused.

The headless `annotation_probe` is a thin CLI over `wsi-dicom-annotations` and
remains a separate process/reporting boundary.
Profiled GeoJSON becomes ANN, SEG, or Comprehensive 3D SR; profiled TIFF, NPY,
local Zarr, and tiled raster inputs become Parametric Maps. Production
conversion is Rust-owned, while independent harnesses may use other DICOM
implementations as file-level oracles. See [the pathology workflow](ANNOTATION_WORKFLOW.md),
[scheme contract](ANNOTATION_SCHEME_V1.md), and [DICOM-native pathology
conversion](DICOM_NATIVE_CONVERSION.md).

## Runtime ownership

The eframe UI thread owns `DicomViewerApp`, `SlideCanvas`, `TileRenderer`,
`TileStore`, and `WgpuTileUploader`. Only this thread mutates presentation
state or registers and frees egui textures.

Background work has explicit owners:

- `OpenQueue` permits at most two open workers and retains only the latest
  pending request when both slots are occupied. Study generations prevent a
  superseded result from becoming active.
- `TileLoader` owns the decode workers, their priority queue, in-flight batch
  records, and a bounded result channel sized to twice the worker count.
- `LevelWarmer` owns one worker that prepares deferred per-level metadata. It
  reprioritizes pending levels for the active study without cancelling useful
  preparation from the same study.
- `AnnotationLoadJob` owns one-shot lazy ANN/SEG decoding. `WorkspaceExportJob`
  owns the single cancellable export slot for workspace, GeoJSON, ANN/SEG/SR,
  and atomically published PM bundles.
- Profiled-GeoJSON, structured-report, and raster sessions use one narrow
  background-worker primitive for validation and semantic reread. Only the UI
  thread transfers imported sessions into workspace-owned external payloads
  and creates heatmap textures.

`TileLoader` and `LevelWarmer` set shutdown state, cancel active work, notify
their condition variables, and join their workers in `Drop`.

## Tile demand and state

`TileKey` is cache identity: `(study generation, level, coordinate)`.
`DemandEpoch` is scheduler revision and changes only when the canonical
key-to-lane map changes. Distance affects ordering within a lane, not cache
identity or demand revision.

Each paint builds one complete `FrameTileDemand`, deduplicates each key into
its strongest lane, applies the 8,192-job ceiling, publishes under one loader
lock, and wakes workers once. Lane priority is:

1. `Visible`
2. `TransitionTarget`
3. `Fallback`
4. `Overview`
5. `Prefetch`

Current visible work is retained first when demand must be capped. Workers do
not admit fallback or prefetch while foreground work is waiting. With more
than one worker, only one transition batch may run alongside visible work;
with one worker, visible and transition batches alternate, starting with
visible.

`TileState` owns the lifecycle of a cache entry:

```text
missing -> queued -> decoding -> decoded -> uploading -> ready
                                      \             \-> failed
                                       \-> failed
```

The queued and decoding states retain `TileReadMode`, so reprioritization does
not erase a pending CPU fallback. Cancellation removes pending state without
creating a failure. Failed target tiles are not painted; a ready coarser level
remains visible.

A failed multi-tile read or wrong result cardinality receives one individual
attempt per tile. An already-single read and a decoder panic are not retried.
A failed Metal import permits one CPU read on the following frame demand.

## Cancellation and stale results

Each admitted batch owns a `ReadCancellationToken` plus its epoch, lane, and
keys. A newer demand cancels an older batch only when none of its keys remain
useful. Clearing or replacing a study cancels all queued and in-flight work.

Source cancellation is cooperative. A running JPEG or JPEG 2000 kernel is not
preempted, but the viewer admits no additional fallback, retry, or batch
element after it observes cancellation. An older successful result is accepted
only if it is still demanded or belongs to the reserved overview; otherwise it
is discarded as obsolete work.

Messages and diagnostics that carry a study generation are filtered before
they affect current presentation state or statistics.

## Cache invariants

`ViewerCacheBudgets` defines three independent byte budgets:

| Profile | Viewer decoded/texture cache | wsi-rs shared tile cache | wsi-rs display cache |
| --- | ---: | ---: | ---: |
| `balanced` | 256 MiB | 128 MiB | 32 MiB |
| `large` | 512 MiB | 256 MiB | 64 MiB |

`TileStore` accounts decoded source bytes, ready texture bytes, and synchronous
upload overlap incrementally and keeps the viewer cache at or below its
configured ceiling. An uploading entry reserves source plus destination bytes
and cannot be evicted until the uploader returns ownership. Every upload
outcome, including missing or surplus results, reconciles that reservation to
ready, deferred decoded, one CPU retry, or terminal failure. Eviction order is
unprotected LRU, frame-pinned LRU, then overview-reserved LRU.

`TileFootprint` is the authoritative checked representation for tile-memory
calculations. It derives actual edge-tile dimensions and records decoded-source,
CPU-RGBA, final-texture, temporary-conversion, upload-peak, and in-flight
reservation bytes. Loader admission, cache accounting and reconciliation, CPU
upload validation, overview planning, and fallback-size checks consume this
representation rather than repeating dimension arithmetic. CPU RGBA bytes
describe the decoded source allocation and are not counted twice in the upload
peak; the current CPU and Metal routes require no separately allocated temporary
conversion buffer.

The overview plan selects center-nearest tiles from the coarsest regular level
up to 32 MiB. Those keys receive stronger cache protection than current-frame
pins so a close-up-to-fit transition can reuse them. The hard viewer ceiling
still takes precedence over protection.

Queued, decoding, decoded-awaiting-upload, and uploading tiles count as
loading. Failed tiles remain uncovered but do not keep the loading indicator
active. Checked edge dimensions reject a tile before decode when its final
RGBA texture cannot fit; an actual decoded or upload-peak overrun is a
persistent failure and that key is not decoded again.

The ceiling is deliberately scoped to store-owned decoded data, ready
textures, synchronous source-plus-destination upload overlap, and one shared
decoded-byte reservation across all worker batches. Reservations remain held
until the UI has transferred every batch result into the byte-accounted store.
Decoder scratch, wsi-rs source caches, and driver overhead are outside it.

## Upload and color management

The UI thread builds one ordered upload plan per paint. Foreground capacity is
reserved for visible work, with at most two transition uploads during
interaction. CPU work is time-budgeted and may be deferred without another
source read. Empty and CPU-only batches create no command encoder; a batch
with successful Metal work uses one encoder and one submission.

`dicom-viewer-core` selects the source ICC profile and owns the CPU transform
and color-management summary. CPU tiles are converted directly with
LittleCMS. On the Metal path, the uploader owns the per-profile LUT texture
used by the existing RGB-to-RGBA compute pass. A profile that cannot meet the
validated LUT error bound forces the correct CPU color path. A malformed
profile leaves pixels uncorrected and produces a persistent warning.

The exhaustive `256^3` LUT proof is cached process-locally in a 16-entry LRU
keyed by profile hash, LUT edge, error bound, and validation-algorithm version.
Concurrent requests for one key share one proof, and both pass and fail
results are cached. A cold proof uses at most four scoped workers, each with an
independent color transform. Cache, worker, transform, or proof infrastructure
failure is visible and forces CPU color conversion; it never accepts an
unproved LUT.

The Metal uploader keeps at most 16 GPU copies of proved LUTs in an LRU and
clears them when the study is replaced. A LUT evicted or cleared while an
upload batch is being encoded remains explicitly owned through submission.

## Metal boundary

The application and core crates use `forbid(unsafe_code)`. The
`metal-wgpu-interop` crate validates the wgpu backend, exact Metal device,
pixel format, offset, pitch, allocation length, and shader addressing before
adopting a buffer. Imported storage is read-only and independently retained.
Raw handles do not escape this crate.

CPU fallback is a normal tile-source outcome, not a second presentation
backend. wgpu remains the only renderer. On macOS the renderer-local Metal
path remains preferred. On other platforms, `--features cuda` creates reusable
wsi-rs CUDA sessions for compressed decode. A CUDA tile is converted only by
the wsi-rs `download_cpu` boundary, then follows the existing RGBA, ICC, cache,
and wgpu upload path. CUDA download failure permits exactly one ordered CPU
retry. No CUDA allocation or surface internals escape into the viewer.

## Diagnostics

Diagnostics are disabled unless `DICOM_VIEWER_DEBUG_STATS=1`. When disabled,
the pipeline does not take diagnostic timers or allocate diagnostic samples.
When enabled, schema-v4 `pipeline_window` records use a trailing one-second
window and `interaction_summary` records close a settled zoom gesture.

Timing names are literal:

- `queue_wait_ms`: enqueue to worker admission; visible-lane samples are also
  reported separately.
- `source_read_ms`: one worker's source call including bounded individual
  recovery.
- `upload_ms`: work performed by the frame's uploader call.
- `eviction_ms`: an over-budget resident-cache eviction pass.
- `level_preparation_ms`: one level-warmer preparation attempt.
- `app_ui_cpu_ms`: CPU time inside `eframe::App::ui`; it excludes egui
  tessellation, GPU execution, and presentation.
- `first_sharp_target_ms`: first ready target tile from the start of the
  continuous gesture.
- `full_target_coverage_ms`: complete target coverage from the last zoom
  input.

Latency distributions include a sample count. Empty distributions use `null`
percentiles rather than reporting zero latency. `tile_probe` measures source
APIs only; its “study-cold” result does not imply a cold operating-system file
cache and is not an end-to-end renderer benchmark.

## Lock and callback rule

Internal mutexes protect scheduler, warmer, source-index, and diagnostic
buffers. Code must not invoke an application-provided callback while holding
one of those locks. State is captured under the lock, the guard is released,
and then notification or callback delivery occurs. The DICOM indexer buffers
controlled-read diagnostics and flushes them after the index operation returns
for this reason.

## Release integration

The viewer workspace resolves `wsi-rs` 0.6.0 at revision `b940ea94` and J2K
0.10.0 at revision `57b6af89` from their upstream Git repositories. CI and
clean checkouts must use the locked revisions without sibling source overlays.
wsi-rs owns codec selection; the viewer must not create a parallel codec or
device-session stack. Metal ownership crosses crate boundaries only as retained
`objc2` protocol objects; the viewer has no `metal-rs` compatibility layer.

Large conformance slides remain local. Repository tests use synthetic fixtures
and opt-in paths for local SVS/DICOM acceptance.

## Known compromises

- Open workers do not have cooperative cancellation. A superseded open may
  finish in the background, but its generation is rejected and the queue keeps
  only the latest pending request.
- `app_ui_cpu_ms` is not end-to-end frame latency. Full tessellation, GPU, and
  presentation timing requires an external capture or additional renderer
  instrumentation.
- Submitted GPU work and running codec kernels cannot be preempted. Cancellation
  prevents subsequent work and publication rather than interrupting a kernel.
- Device-loss recovery and persistent DICOM frame-index sidecars are not part
  of the current viewer.
