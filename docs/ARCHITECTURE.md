# Architecture

This document describes the boundaries and invariants of the desktop viewer.
It is a map of the current implementation, not a roadmap.

## Workspace boundaries

| Area | Responsibility |
| --- | --- |
| `apps/dicom-viewer` | eframe application state, camera and measurement interaction, demand planning, worker ownership, tile cache, upload, and presentation |
| `crates/dicom-viewer-core` | wsi-rs façade, input inspection, canonical slide geometry, renderer-facing tile conversion, and color-management policy |
| `crates/metal-wgpu-interop` | Audited macOS-only ownership boundary for importing immutable J2K Metal allocations into the renderer's wgpu device |
| `wsi-rs` | Format detection, metadata, source I/O, tile extraction, codec dispatch, and source/display caches |

The app crate is the composition root. Core does not own UI state, and the
interop crate is the only viewer crate allowed to perform raw wgpu-hal or
Objective-C operations.

## Runtime ownership

The eframe UI thread owns `DicomViewerApp`, `SlideCanvas`, `TileRenderer`,
`TileStore`, and `WgpuTileUploader`. Only this thread mutates presentation
state or registers and frees egui textures.

Background work has three explicit owners:

- `OpenQueue` permits at most two open workers and retains only the latest
  pending request when both slots are occupied. Study generations prevent a
  superseded result from becoming active.
- `TileLoader` owns the decode workers, their priority queue, in-flight batch
  records, and a bounded result channel sized to twice the worker count.
- `LevelWarmer` owns one worker that prepares deferred per-level metadata. It
  reprioritizes pending levels for the active study without cancelling useful
  preparation from the same study.

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
missing -> queued -> decoding -> decoded -> ready
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

`TileStore` accounts decoded and ready resident bytes incrementally and keeps
the viewer cache at or below its configured ceiling. Eviction order is
unprotected LRU, frame-pinned LRU, then overview-reserved LRU.

The overview plan selects center-nearest tiles from the coarsest regular level
up to 32 MiB. Those keys receive stronger cache protection than current-frame
pins so a close-up-to-fit transition can reuse them. The hard viewer ceiling
still takes precedence over protection.

Queued, decoding, and decoded-awaiting-upload tiles count as loading. Failed
tiles remain uncovered but do not keep the loading indicator active.

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

## Metal boundary

The application and core crates use `forbid(unsafe_code)`. The
`metal-wgpu-interop` crate validates the wgpu backend, exact Metal device,
pixel format, offset, pitch, allocation length, and shader addressing before
adopting a buffer. Imported storage is read-only and independently retained.
Raw handles do not escape this crate.

CPU fallback is a normal tile-source outcome, not a second presentation
backend. wgpu remains the only renderer. The `cuda` feature exposes decode
capability through wsi-rs but does not add CUDA-to-wgpu interop.

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

The viewer workspace expects sibling `wsi-rs` 0.5.2 and `j2k` 0.7.4
checkouts. Its direct dependencies are exact-versioned and CI substitutes the
corresponding tags. wsi-rs owns codec selection; the viewer must not create a
parallel codec or device-session stack.

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
