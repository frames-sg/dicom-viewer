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
