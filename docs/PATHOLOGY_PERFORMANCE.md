# Pathology overlay performance characterization

The pathology renderer continues to use egui Painter/epaint over the existing
wgpu renderer. No raw-wgpu overlay path was added because the characterized
epaint path remained bounded after spatial lookup and point batching.

## Reproducible workload

Run the ignored optimized test from the repository root:

```sh
cargo test --release -p dicom-viewer --bin dicom-viewer \
  pathology_overlay_release_characterization --locked -- \
  --ignored --nocapture
```

The retained workload creates one validated `WorkspaceDocument` containing:

- 50,000 visible controlled cell detections;
- 20,000 visible independent neoplasm regions;
- 20 stored vertices per region;
- a 16,384 × 16,384 base-pixel source;
- a 1920 × 1080 viewport fit to source height;
- 15 samples for each candidate object/vertex budget.

It builds the production R-tree/render index, runs the production viewport
query, performs 4-pixel screen-bin aggregation, builds one compact colored
point mesh, applies display-only subpixel contour decimation, and submits the
resulting epaint shapes. `std::hint::black_box` retains the output.

## Recorded result

Characterized 2026-08-15 on:

- macOS Darwin 25.5.0, arm64;
- Apple M4 Pro, 48 GiB RAM, Metal 4;
- rustc 1.96.0 (`ac68faa20`, LLVM 22.1.2);
- release profile with fat LTO and one codegen unit.

### Historical evidence limitation

The original run did not record the exact repository commit plus worktree
identity, fixture checksum, peak-memory observation, or a retained raw sample
artifact. The deterministic tests below establish output invariants, but they do
not reconstruct the precise measured source tree or provide pixel/state parity
and peak-memory evidence for that run. Treat these numbers as historical sizing
evidence only, not as a reproducible current baseline or an end-to-end viewer
performance claim.

Before using this result to retain or change a production limit, rerun the
documented command and record the exact commit/worktree state, raw 15-sample
series, output checksum or equivalent parity evidence, and peak RSS. New tile
pipeline experiments must record equivalent before/after workloads, repeated samples,
output parity, and peak memory before making performance claims.

| Detailed objects | Detailed vertices | p50 | p95 / max |
| ---: | ---: | ---: | ---: |
| 4,000 | 80,000 | 14.061 ms | 15.406 ms |
| 8,000 | 160,000 | 15.431 ms | 15.860 ms |
| 12,000 | 240,000 | 16.447 ms | 17.374 ms |
| 16,000 | 320,000 | 17.574 ms | 19.670 ms |

The production limits are **8,000 detailed non-point objects** and **160,000
detailed vertices**. This was the highest characterized pair whose p95 stayed
below one 60 Hz CPU-frame interval on this workload. Selected objects bypass
both limits so selection never disappears. Every visible point bypasses the
detailed-object cap and participates in screen-bin aggregation; bins remain
separate by displayed color and are emitted as one mesh.

The render index owns immutable render records, so drawing does not perform a
linear document search for each visible object. R-tree queries return only
viewport-intersecting records. Unselected contour vertices closer than 0.5
screen pixels are removed only from temporary display geometry; selected,
stored, and exported coordinates remain exact. Specialized mask and heatmap
renderers remain separate layer renderers.

## CI assertions and limits

Normal CI does not assert wall-clock duration. Deterministic tests instead
verify that:

- viewport queries exclude nonintersecting objects;
- all visible points survive the detail cap for aggregation;
- selected objects survive zero detail budget;
- selected contours retain all vertices;
- display decimation never mutates stored coordinates;
- dense point bins form one valid mesh with bounded vertices/indices.

This characterization measures CPU-side overlay query and epaint submission;
it is not end-to-end frame latency and does not include egui tessellation,
wgpu execution, display presentation, slide tile work, or operating-system
scheduling. The interactive release gate in [RELEASE.md](RELEASE.md) still
requires representative fixtures, external presentation timing, latency and
memory limits, and sustained interaction. These measurements do not support a
“real-time” clinical claim.

## Workspace and tile CPU regression workloads

Workspace snapshots share immutable tracking/text and layer storage. A successful
validation proof survives only setters that validate their replacement locally
and preserve global identity and resource-count invariants. Deserialization and
structural changes discard that proof. Geometry edits replace an opaque render
identity; metadata and presentation changes preserve it. Runtime publication
checks that identity before reusing a current spatial index, including undo and
redo. Handle movement updates one spatial record after validating the candidate;
a stale or absent record takes the full-rebuild path.

Segment composition follows its immutable primitive snapshot. Its retained
coordinates are included in the existing conservative undo-memory estimate.
Geometry changes invalidate that estimate even when input point counts are
unchanged. Findings rows are cached by document snapshot, so stable redraws do
not repeatedly format and sort all findings. The first redraw after an edit still
materializes the rows; this is not a claim of constant-time row construction.

Imported layers own spatial lookup data for their loaded payload. Canonical
read-only coordinates and fractional-frame maxima are prepared once. Queries
preserve source order for overlapping colors/opacity and include primitives whose
bounds cross the viewport even when their vertices lie outside it. Replacing a
payload resets its prepared data; replacing the study creates a new runtime.
Dense visible content still incurs its actual drawing cost.

The loader preserves sequence values for unchanged priorities and defers
incompatible heap entries once per batch. Cache eviction heapifies candidates,
then removes only the needed victims in the existing protection/recency order.
This favors small evictions; removing much of the cache can be slower than a full
sort. Existing tile reservations and uploading-entry exclusions remain in force.

RGB8 expansion has a safe, fixed-layout path at the viewer's RGBA boundary, with
the source adapter retained for other layouts/sample types. Color management still
uses exact LittleCMS transforms and preserves alpha. Worker counts, batch defaults,
and texture allocation policy require broader fixture and interaction evidence
before tuning; isolated throughput measurements alone do not determine them.

Additional opt-in commands (all timings require release builds):

```sh
cargo test --release -p dicom-viewer --bin dicom-viewer --locked \
  cpu_workspace_release_characterization -- --ignored --nocapture
cargo test --release -p dicom-viewer --bin dicom-viewer --locked \
  eviction_cpu_performance -- --ignored --nocapture
cargo test --release -p dicom-viewer --bin dicom-viewer --locked \
  external_overlay_cpu_performance -- --ignored --nocapture
cargo test --release -p dicom-viewer --bin dicom-viewer --locked \
  upload_cpu_performance -- --ignored --nocapture
cargo test --release -p dicom-viewer-core --lib --locked \
  color_cpu_performance -- --ignored --nocapture
```

`loader_cpu_performance` and `compatible_queue_release_characterization` require
`DICOM_VIEWER_WSI_FIXTURE` pointing at a trusted WSI file or DICOM folder. The
loader workload checks all returned RGBA bytes for 64 distributed tiles, varying
workers (1/2/4) and batch size (1/4/8/16), with five samples per setting. Repeat
separately with `DICOM_VIEWER_JP2K_THREADS=1`, `4`, and the default. It measures a
warm source and excludes study open and worker startup; it is not an interaction
or cancellation latency benchmark. The upload workload requires a real wgpu
adapter and checks texture readback and registration release outside its timer.

`cpu_metadata_parser_release_characterization` requires the fixture variable to
name one DICOM file. It compares complete emitted metadata outside the timers;
the eager-parser-only case performs different safety work from production and
must not be represented as an equivalent replacement. Keep local raw samples,
hardware/configuration, relevant checkout changes, and memory observations in
`.local-docs/` when using these workloads to support an optimization decision.

## Metal upload characterization

The upload path prepares independent resident RGB8 conversions and reuses
immutable LUT and destination views. This reduces
per-tile setup without changing the conversion shader, source decode, upload
budget, or color tolerance. It retains independent textures and the portable CPU
path; texture pooling would require separate evidence about allocation cost and
resource lifetime.

Conversions retain one compute pass per tile. An eight-tile single-pass candidate
reduced host setup in some workloads, but GPU timestamp comparisons showed
unstable results and slower large-tile samples. That evidence did not justify
changing pass grouping. The retained comparison below can reassess this choice
on representative hardware and workloads.

```sh
cargo test --release -p dicom-viewer --bin dicom-viewer --locked \
  metal_upload_release_characterization -- --ignored --nocapture
cargo test --release -p dicom-viewer --bin dicom-viewer --locked \
  metal_pass_gpu_timestamp_characterization -- --ignored --nocapture
```

Both commands require macOS and a real Metal adapter. The first measures 15
samples of 1/8 resident tiles at 64/256/1024 pixels, with and without a 65-edge
identity LUT. Compare the per-sample sum of preparation and submission/registration
times when encoding moves between those stages. Host completion includes device
polling and scheduling; it is not GPU execution time. Readback checks every output
byte, exactly without a LUT and within one RGB code value with the quantized
identity LUT; alpha is exact. Registration release is also checked outside the
timer. Capture peak RSS for the whole workload when comparing allocation changes.

The second isolates one pass versus one pass per tile using identical prepared
resources and shader, eight 256/1024-pixel tiles, and 15 samples per strategy. It
alternates strategy order and timestamps only the first pass beginning and last
pass ending. Fresh query storage and a completion wait before resolution avoid
reusing an earlier result. Nonzero, increasing timestamps and a GPU interval no
longer than host completion are required before reporting a sample. Readback
validates both strategies. A debug run performs only a small API/parity smoke
check. Timestamp instrumentation can affect execution; these results do not
measure source decode, presentation, or interaction latency.

For a trusted fixture, set `DICOM_VIEWER_WSI_FIXTURE` and run
`metal_source_route_diagnostics` with the same app test command. Confirm the
resolved backend before attributing a result to Metal. The core test
`metal_lut_resolution_characterization` compares the production color validator
at LUT edges 65/86/129 using the fixture profile and synthetic gamma profiles.
A denser interpolated LUT is only a candidate: it must still meet the existing
two-code-value bound. Production retains a proved 65³ table when possible and
uses an exact 256³ RGB8 lookup when interpolation fails. The latter is not a
relaxed approximation: every entry is the direct LittleCMS result, loaded by
integer texel address. See the memory bounds in [ARCHITECTURE.md](ARCHITECTURE.md).

The exact-color regression and complete pipeline comparison run with:

```sh
DICOM_VIEWER_WSI_FIXTURE=/path/to/profiled/dicom/folder \
  cargo test --release -p dicom-viewer --bin dicom-viewer --locked \
  app::tile::upload::color_tests -- --ignored --nocapture --test-threads=1
```

The fixture must exercise exact color admission and contain at least eight full
central tiles per level plus cropped bottom-right tiles. The GPU lookup test
reads back all 16,777,216 RGB inputs through a discontinuous table and requires
bit-exact RGBA output through both resident RGB and CPU-uploaded RGBA inputs,
including after cache clearing. The fixture regression checks deferred Metal
color for CPU-decoded full and cropped tiles, and bit-exact final agreement with
the CPU reference. Native GPU JPEG2000 decoding was substantially slower on this
fixture, so exact-color admission preserves CPU decompression.

The pipeline comparison alternates CPU and Metal color over 15 samples per level using
eight central tiles and separate source caches. It times source reads, color
conversion, texture upload, and completion polling, then checks every pixel
outside the timer. Both paths flush pending queue writes before completion;
source-only render reads can return CPU-resident pixels with GPU color still
pending and are not equivalent to completed CPU RGBA reads. Record the first
sample separately from warm medians and capture
process memory with `/usr/bin/time -l` on the built test executable. These timings
exclude display presentation and do not establish gesture latency.

## Native Metal JPEG2000 decode diagnosis

The September 2026 investigation found a subsampling performance cliff in the
pinned codec. The measured DICOM frames have 4:2:2 components: 256×256 luminance
and two 128×256 chroma components. Direct color planning rejects them with
`ComponentUnitSampled`; the prepared batch API also reports
`ComponentSubsampling`. The legacy device API then uses synchronous component,
subband, and code-block callbacks. A Metal System Trace recorded 4,410 command
buffer submissions for 72 tiles. Stack sampling attributed about 95% of fallback
decode samples to classic entropy decoding. The plain entropy kernel decodes a
code block in one lane; grouping a tiny number of blocks at a time fails to
expose the independent work across the requested tiles.

The local codec now supplies a separate component-grid planner, batches matching
component graphs across images, and replicates chroma samples on the GPU before
packing RGB/RGBA. The existing full-resolution planner still rejects subsampling;
its contract is preserved. The sampled batch regression requires one submission,
checks exact reversible output at odd dimensions, and covers distinct/repeated
inputs. Real irreversible DICOM tiles retain the two-code-value pixel bound.
Source-only warm medians on the same fixture are now approximately 21/25/27 ms
per eight tiles, compared with approximately 515/827/704 ms on the earlier codec.
These are source timings, excluding ICC and presentation. CPU decoding remains
faster on these small classic JPEG2000 tiles, so exact-color admission retains
the CPU decode plus Metal color route.
Apple's [command buffer guidance](https://developer.apple.com/library/archive/documentation/3DDrawing/Conceptual/MTLBestPracticesGuide/CommandBuffers.html)
and [shared-memory synchronization requirements](https://developer.apple.com/documentation/metal/mtlstoragemode/shared)
apply even on unified-memory hardware. The general entropy-kernel alternative
was slower and was rejected. Other Apple GPUs and larger tiles need separate
measurements before changing decoder selection.

While publishing is paused, `.cargo/config.toml` overrides the J2K packages with
the sibling `../j2k` checkout so normal local builds use the fix. All J2K package
identities use the same checkout. The Git revision declarations are unchanged;
the overrides and local-source lock entries must be replaced together by a
published revision when publishing resumes.

The source-only diagnostic bypasses ICC routing without changing viewer policy:

```sh
DICOM_VIEWER_WSI_FIXTURE=/path/to/dicom/folder \
  cargo test --release -p dicom-viewer-core --locked \
  local_native_metal_decode_characterization -- --ignored --nocapture --test-threads=1
```

It requires regular levels with at least nine columns and three rows, validates
resident output count and dimensions, and reports direct-plan admission.
`DICOM_VIEWER_DECODE_SAMPLES` controls repeats (default three).
`DICOM_VIEWER_DECODE_EXPORT` optionally names an existing directory for the exact
compressed tile inputs used in codec-level differential checks. This diagnostic
excludes ICC, renderer upload, and presentation; its timings are not complete
pipeline comparisons. The local investigation report includes pixel comparisons,
trace extraction, hardware, commands, and remaining profiling limits.
