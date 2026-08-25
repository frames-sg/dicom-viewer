# Baseline

## Repository and working tree

- Repository: `/Users/user/Bench/frames/dicom-viewer`
- Audit anchor and starting HEAD: `87aa73d8cf5223ef6ba674e2ef7747453df06442`
- Branch: `main`, tracking `origin/main`
- There are no repository- or directory-level `AGENTS.md` files below the target repository.
- The target is a three-member Rust workspace. It also has a path dependency on the sibling `/Users/user/Bench/frames/wsi-dicom-annotations`, whose dirty API currently participates in the baseline compile failure.
- The worktree was already substantially dirty when the refactor plan was created. No reset, clean, checkout, commit, or remote mutation is permitted.

Starting tracked diff statistic:

```text
29 files changed, 2764 insertions(+), 3193 deletions(-)
```

The pre-existing changes add a pathology workspace, background/export/raster/report
jobs, annotation probe, core annotation model, supply-chain patches, and a mechanical
split of tile loader queue/decode/worker code. They also delete the former monolithic
annotation and measurement modules. These changes are not part of the refactor
checkpoint and must be preserved.

## Toolchain and machine

- Rust: `rustc 1.96.0 (ac68faa20 2026-05-25)`, host `aarch64-apple-darwin`, LLVM 22.1.2.
- Cargo: `1.96.0 (30a34c682 2026-05-25)`.
- OS: macOS 26.5.2 (25F84), Darwin 25.5.0.
- CPU: Apple M4 Pro, 12 logical/physical CPUs.
- Memory: 51,539,607,552 bytes (48 GiB).
- GPU: Apple M4 Pro, 16 GPU cores, Metal 4.
- Display: built-in 3024×1964 Retina.

## Workspace members and dependency revisions

| Member | Kind | Responsibility |
| --- | --- | --- |
| `apps/dicom-viewer` | binaries `dicom-viewer`, `tile_probe`, `annotation_probe` | native eframe UI, tile pipeline, probes |
| `crates/dicom-viewer-core` | library | wsi-rs façade, inspection, color, viewer models, annotation adapters |
| `crates/metal-wgpu-interop` | macOS library | narrow unsafe Metal/wgpu import boundary |

Important locked declarations:

- workspace Rust version 1.96, edition 2021;
- `wsi-rs = 0.5.2` from crates.io; Metal feature on macOS, CUDA feature when selected;
- `j2k-core`, `j2k-native`, `j2k-metal-support = 0.8.0`;
- `wgpu = 29.0.3`, `egui-wgpu = 0.34.3`, `eframe = 0.34.2`;
- direct path dependency `wsi-dicom-annotations = 0.1.0` at `../wsi-dicom-annotations`;
- local `[patch.crates-io]` entries for audited `vendor/lru` and `vendor/wayland-scanner` security backports.

## Supported backend and feature matrix

| Platform | Default | Optional | Runtime evidence available here |
| --- | --- | --- | --- |
| macOS | wgpu presentation; renderer-local Metal decode/import when supported; CPU fallback | forced CPU through config | compile/test host available; real WSI fixture absent |
| Linux | wgpu presentation; CPU decode | `--features cuda`, checked host download, no CUDA-to-wgpu interop | unavailable locally |
| Windows | wgpu presentation; CPU decode | none documented | unavailable locally |

`--all-features` is not a portable release command. CI runs default checks on macOS,
Windows, and Linux; CUDA compilation is Linux-only, and runtime CUDA validation uses a
self-hosted CUDA runner.

## Repository-supported validation commands

```sh
cargo fmt --all -- --check
cargo metadata --locked --format-version 1
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo check --workspace --all-targets --locked --features cuda   # Linux CI
cargo test -p dicom-viewer-core --locked macos_metal_options_return_resident_tiles_for_synthetic_dicom_htj2k   # macOS
cargo build --workspace --release --locked
cargo machete
cargo audit --deny unsound
cargo deny check advisories bans licenses sources
```

CUDA runtime workflow additionally runs in the sibling repositories:

```sh
cargo test --locked --features cuda --lib cuda_download_cpu
cargo test -p dicom-viewer-core --locked --features cuda cuda_viewer_download_matches_strict_cpu_for_synthetic_dicom_htj2k
```

Probe command (requires a user-supplied research fixture):

```sh
cargo run --release -p dicom-viewer --bin tile_probe -- \
  --trials 5 --json --api controlled-render --backend auto --batch-size 8 sample.svs
```

Manual workflows are in `MANUAL_ACCEPTANCE.md`. The repository has no separate
documentation build command; library doctests are included in workspace tests.

## Baseline command results

| Command | Result |
| --- | --- |
| `git diff --check` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `cargo test --workspace --all-targets --locked` | FAIL during app compilation after 19.1 s: `workspace.rs:1266` calls missing `vectorized_annotation_groups`; dirty path dependency exposes `vectorized_annotations(policy)` |

The baseline blocker was then repaired in the viewer with the current typed API and an
explicit `AllowLoss` policy, preserving the old raster-overlay projection semantics.
Post-repair validation:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo metadata --locked --format-version 1` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS; upstream `block 0.1.6` future-incompatibility notice remains |
| `cargo test --workspace --all-targets --locked` | PASS: 352 passed, 2 ignored, 0 failed |
| `cargo test -p dicom-viewer-core --locked macos_metal_options_return_resident_tiles_for_synthetic_dicom_htj2k` | PASS: 1 passed |
| `cargo build --workspace --release --locked` | PASS in 2m32s |
| `cargo machete` | PASS; no unused dependencies |
| `cargo audit --deny unsound` | PASS with two policy-allowed unmaintained warnings (`encoding`, `paste`) |
| `cargo deny check advisories bans licenses sources` | PASS; duplicate-version warnings are allowed by policy |

CUDA compilation/runtime, Linux/Windows checks, real-fixture probes, and manual UI
acceptance were not run on this macOS checkpoint.

## Fixture inventory

- No committed real WSI/DICOM/JPEG/JPEG-2000 performance files were found.
- Tile-loader tests generate a tiny HTJ2K RGB8 codestream and temporary source.
- Core tests synthesize Part 10 DICOM WSI metadata, malformed headers, oversized values, and truncated metadata.
- Tile/canvas/store/upload tests use synthetic keys, levels, decoded buffers, fake textures/uploaders, cancellation tokens, huge grids, edge tiles, and malformed cardinalities.
- Optional local parity tests use `DICOM_VIEWER_WSI_FIXTURE`; it is not configured in the baseline.
- Missing fixture classes: multiple optical paths, multiple focal planes, concatenated instances, sparse/non-full tiling, high-coordinate end-to-end geometry, and representative real cold/warm performance sources.

## Telemetry schema and benchmark surface

- Current public diagnostics are schema v4 JSONL `pipeline_window` and `interaction_summary` records emitted once per second to stderr when `DICOM_VIEWER_DEBUG_STATS=1`.
- JSON is manually formatted in `app/tile/stats/json.rs`; state update paths call `eprintln!` in `stats.rs`.
- `app_ui_cpu_ms` excludes egui tessellation, GPU execution, and presentation.
- `tile_probe` measures source APIs, not end-to-end UI or a cold OS page cache.
- No baseline performance number is recorded because no representative fixture was supplied.

## Production hotspot inventory

This is the initial responsibility inventory; `DV-G0-005` remains open until every
production module is classified.

| Path (current LOC) | Current responsibilities / owned state | Synchronization, boundary, extraction direction |
| --- | --- | --- |
| `app/workspace.rs` (2,089) | pathology editor/runtime, external payloads, promotion, selection/drafts | UI-thread state; split only along real editor/promotion/runtime ownership |
| `app/tile/store.rs` (1,674) | all six tile states, bytes, retry/failure, upload transaction, eviction, coverage, drawing | UI-thread owner; wrongly imports egui/presentation; target decoded/ready/failure cache + ledger only |
| `app/tile/stats.rs` (1,542) | samples, counters, interactions, overlays, output | direct stderr; target typed collector/snapshot separate from sink/overlay |
| `app/tile/upload.rs` (1,389) | CPU/Metal preparation, WGSL, LUTs, wgpu submission, egui registration, config | UI-thread GPU boundary; target CPU/Metal/registration/color setup responsibilities |
| `app/canvas.rs` (1,289) | level policy, frame plan, warming, demand/poll construction, cache protection, draw orchestration | UI thread; target pure plan + orchestration + painter |
| `app/tile.rs` (960) | domain types, renderer/coordinator-like orchestration, demand canonicalization, acceptance sets, upload planning | UI thread; target domain/demand/policy/coordinator split |
| `app/level_warmer.rs` (918) | queue, worker lifecycle, preparation, diagnostics | `Arc<Mutex<_>>`, `Condvar`, bounded result channel; share only lifecycle/diagnostic primitives |
| `app.rs` (807) | app orchestration, open flow/status, pathology actions, UI commands | UI thread plus open queue; retain high-level orchestration only |
| `app/tile/loader.rs` (425) | scheduler facade, shared queue/in-flight state, worker ownership, configuration | `Arc<Mutex<_>>`, `Condvar`, bounded result channel; dirty tree extracted queue/decode/worker files |
| `app/tile/loader/queue.rs` (342) | admission, fairness, batch formation, diagnostics buffer setup | shared loader lock; target scheduler owner |
| `app/tile/loader/decode.rs` (295) | source reads, cardinality, panic containment, CUDA/CPU recovery | no cache mutation; target typed decoder outcomes |
| `app/tile/loader/worker.rs` (104) | named spawn and worker loop | operates loader shared state; target worker-pool lifecycle owner |
| `app/open_job.rs` (188) | max-two open queue, latest pending, generation filtering | unbounded `mpsc::channel`, detached thread handles; target cooperative managed tasks |
| `core/model.rs` (691) | errors, summaries, config/budgets, tile/color/identity | core public surface; split by responsibility while preserving re-exports |
| `core/inspection/dicom.rs` (630) | enumeration, bounded preflight, fact extraction, aggregation/warnings | synchronous I/O; multidimensional DICOM model incomplete |
| `core/color.rs` (889) | ICC selection, CPU transform, LUT proof/cache/policy | cohesive, security/performance-sensitive; do not split by size alone |
| `metal-wgpu-interop/macos.rs` (448) | raw Metal/wgpu validation/import | only allowed unsafe boundary; preserve narrow contract |

## Current module dependency/ownership map

```text
DicomViewerApp
  -> SlideCanvas
       -> TileRenderer
            -> TileLoader -> queue/decode/worker -> ViewerStudy/wsi-rs
            -> TileStore -> egui painting + cache + lifecycle + accounting
            -> WgpuTileUploader -> ViewerOpenOptions/env + Metal/wgpu + egui registration
            -> PipelineStats -> JSON formatting + stderr + overlay
       -> LevelWarmer -> ViewerStudy/wsi-rs
       -> TileFramePlan + FrameTileDemand + TilePollRequest
  -> OpenQueue/OpenJob -> ViewerStudy::open
```

The target ownership and transition map is in `TILE_STATE_MODEL.md`.
