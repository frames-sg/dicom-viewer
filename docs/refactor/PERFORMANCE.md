# Performance Evidence

No performance gain is claimed in this document without a reproducible before/after
record and parity evidence.

## Environment

- Date: 2026-08-20
- Host: Apple M4 Pro, 12 CPU cores, 16 GPU cores, 48 GiB RAM
- OS: macOS 26.5.2 (25F84)
- Rust: 1.96.0; release profile uses fat LTO, one codegen unit, symbols stripped, opt-level 3
- Viewer HEAD: `87aa73d8cf5223ef6ba674e2ef7747453df06442` plus a large pre-existing dirty worktree
- Available runtime backends: CPU and Metal-capable host; CUDA unavailable
- Real benchmark fixture: none configured

## Required experiment record

Every experiment must record:

1. ID and hypothesis.
2. Affected code and exact diff/checkpoint.
3. Fixture path label and SHA-256 without exposing PHI-bearing paths in public telemetry.
4. Source format/workload and backend/feature.
5. Exact command, hardware/OS/toolchain, release profile.
6. Warmup and sample count.
7. Baseline p50/p95/p99/max or other declared statistic.
8. After result and delta.
9. Pixel/state/telemetry parity evidence.
10. Peak RSS/resident/pinned/GPU submission effect.
11. Retain or reject decision and known cliffs.

## Benchmark definitions

- **DV-BENCH-001 cold open:** fresh process and independent study; does not claim OS page-cache eviction unless externally controlled.
- **DV-BENCH-002 warm open:** repeated independent study open with warm filesystem caches.
- **DV-BENCH-003 stationary frame plan:** identical study/viewport/camera/policy/cache generation, repeated plan construction.
- **DV-BENCH-004 cache eviction:** deterministic ready/decoded mixes at 100, 1,000, and 10,000 entries with equal protected sets and checksums.
- **DV-BENCH-005 interaction:** initial fit, one zoom, continuous zoom, rapid reversal, pan while incomplete, resize, revisit, memory pressure.
- **DV-BENCH-006 upload:** equal CPU or Metal inputs, identical texture bytes and registration results, recording encoder/submission counts.
- **DV-BENCH-007 telemetry:** disabled, window summaries, interaction summaries, full debug using the same interaction trace.
- **DV-BENCH-008 thread matrix:** outer workers 1/2/4 × inner JP2K 1/2/4/bounded-auto where the upstream API truly supports it.

## Baseline data

No end-to-end performance baseline exists yet. The initial
`cargo test --workspace --all-targets --locked` attempt failed during compilation after
19.1 seconds; the compatibility issue was subsequently repaired and the workspace tests
passed. Neither build/test duration is a viewer performance result.

Repository `tile_probe` is available for source API timing only. Its “study-cold” batch
does not flush the operating-system cache and must not be reported as cold application
open or end-to-end frame latency.

`docs/PATHOLOGY_PERFORMANCE.md` contains a 2026-08-15 CPU-side overlay
characterization. Its exact commit/worktree identity, raw samples, parity artifact, and
peak-memory result were not retained, so it is classified as historical sizing evidence
rather than a reproducible baseline under this program. It must be rerun before it is
used to justify a new optimization or production-limit change.

## Candidate experiments

| ID | Candidate | Status | Current evidence / next gate |
| --- | --- | --- | --- |
| DV-PERF-EXP-001 | frame-plan caching | OPEN | add deterministic stationary benchmark after `DemandSnapshot`/pure plan |
| DV-PERF-EXP-002 | scratch-buffer/allocation reuse | OPEN | profile allocation sites; no unsafe pools |
| DV-PERF-EXP-003 | indexed eviction | OPEN | compare against current exact behavior at 100/1k/10k |
| DV-PERF-EXP-004 | adaptive memory policy | OPEN | retain hard ceiling and explicit profiles |
| DV-PERF-EXP-005 | decoder batch matrix | BLOCKED | requires representative fixture |
| DV-PERF-EXP-006 | queue lane A/B | BLOCKED | requires interaction telemetry fixture |
| DV-PERF-EXP-007 | earlier stale cancellation | BLOCKED | requires interaction trace/fixture |
| DV-PERF-EXP-008 | CPU upload path | OPEN | instrument equal outputs and encoder/submission count |
| DV-PERF-EXP-009 | Metal resource reuse | BLOCKED | profile before adding rings/pools/caches |
| DV-PERF-EXP-010 | upload submission batching | OPEN | current code claims one Metal encoder/batch; retain counter evidence |
| DV-PERF-EXP-011 | level warming | BLOCKED | compare disabled/current/lower concurrency with fixture |
| DV-PERF-EXP-012 | DICOM index reuse | BLOCKED | needs DICOM WSI fixture; avoid duplicate viewer-local cache |
| DV-PERF-EXP-013 | compressed payload copies | BLOCKED | trace source through wsi-rs/J2K/device; no unsafe viewer shortcut |
| DV-PERF-EXP-014 | level lookup index | OPEN | first count/measure repeated scans |
| DV-PERF-EXP-015 | typed telemetry overhead | OPEN | benchmark disabled/window/interaction/debug modes |
| DV-PERF-EXP-016 | CUDA host-output path | BLOCKED | macOS host has no CUDA runtime |
| DV-PERF-EXP-017 | external frame-time validation | BLOCKED | capture tooling and acceptance limits not configured |

## Retained and rejected experiments

None yet. Pre-existing implementation choices are not reclassified as measured gains
until reproduced under this document's protocol.
