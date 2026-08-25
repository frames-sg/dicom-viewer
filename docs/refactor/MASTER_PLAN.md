# DICOM Viewer Refactor Master Plan

Audit anchor: `87aa73d8cf5223ef6ba674e2ef7747453df06442`

This is the durable execution plan for the tile-pipeline refactor. Status values are
`OPEN`, `IN PROGRESS`, `BLOCKED`, `DONE`, `SUPERSEDED`, and `NOT APPLICABLE`.
`DONE` requires named implementation, tests, and commands. Pre-existing uncommitted
work is evidence about the current tree, not work credited to this plan.

## G0 — baseline, re-audit, characterization

- **DV-G0-001 — DONE.** Starting `HEAD` equals the audit anchor. The branch is `main` and the large dirty worktree predates this plan. Evidence: `BASELINE.md`, `STATUS.md`; commands `git rev-parse HEAD`, `git status --short --branch`, `git diff --stat`.
- **DV-G0-002 — DONE.** Read the repository root manifest, crate manifests, toolchain file, README, architecture/release/supply-chain documents, CI and CUDA workflows, Reasonix configuration, dependency policy, and searched for all `AGENTS.md` files (none exist below the target repository). Evidence: `BASELINE.md`.
- **DV-G0-003 — DONE.** Repository-supported validation and platform matrix recorded in `BASELINE.md`.
- **DV-G0-004 — DONE.** Initial workspace compilation failed at pre-existing sibling SEG API drift. `WorkspaceRuntime::add_external_segmentation` now uses typed `vectorized_annotations(AllowLoss)`, returns its diagnostics to the caller, and UI status surfaces every blocking projection-loss code. One `frames_viewer_producer` factory supplies explicit Frames/DICOM Viewer identity and per-format series metadata to new ANN/SEG/SR/PM objects, including GeoJSON companion documents. Evidence: workspace import/export, raster, report, and annotation-probe modules; `cargo test --workspace --all-targets --locked` (346 passed, 2 ignored), Clippy with warnings denied, and the optimized workspace build.
- **DV-G0-005 — IN PROGRESS.** Hotspot inventory is recorded; extend it to every production module before closing G0.
- **DV-G0-006 — DONE.** Current end-to-end lifecycle is recorded in `TILE_STATE_MODEL.md`.
- **DV-G0-007 — DONE.** Current and target ownership diagrams are recorded in `TILE_STATE_MODEL.md`.
- **DV-G0-008 — IN PROGRESS.** Existing tests cover most queue, demand, retry, cancellation, upload, accounting, eviction, fallback, and metric behaviors. Restore compilation, run them, then fill the named gaps in `BASELINE.md`.
- **DV-G0-009 — IN PROGRESS.** Synthetic HTJ2K, DICOM metadata, edge-tile, malformed/truncated, cancellation, upload, and large-grid fixtures exist. Multi-optical-path, multi-focal-plane, concatenation, and high-coordinate geometry fixtures remain open.
- **DV-G0-010 — BLOCKED.** No real WSI benchmark fixture is committed or configured. Synthetic test evidence may proceed, but cold/warm viewer measurements require an explicitly supplied research fixture and manual UI run.
- **DV-G0-011 — DONE.** Current finding classification is recorded below and in the supporting inventories.

G0 gate: **OPEN** until the current tree compiles, characterization tests run, the full file inventory is complete, and fixture limitations are recorded with final scope.

## Current audit classification

### Ownership and boundaries

- **DV-AUD-A-001 — OPEN:** distributed lifecycle ownership. `TileStore::TileState` owns `Queued`, `Decoding`, `Decoded`, `Uploading`, `Ready`, and `Failed`; `TileLoader` independently owns queue/in-flight state; `TileRenderer` owns overlapping demand/acceptance sets; canvas builds two demand values.
- **DV-AUD-B-001 — PARTIALLY FIXED:** the dirty tree split loader decode, queue, worker mechanics, and focused tests into submodules, but `loader.rs` still coordinates all three and the authoritative state split is not established.
- **DV-AUD-B-002 — OPEN:** `store.rs` (1,674 LOC), `stats.rs` (1,542), `upload.rs` (1,389), `canvas.rs` (1,289), and `tile.rs` (960) remain tile-pipeline hotspots with mixed responsibilities.
- **DV-AUD-B-003 — OPEN:** `level_warmer.rs` (918) duplicates worker/diagnostic mechanics.
- **DV-AUD-B-004 — PARTIALLY FIXED:** root `app.rs` is 807 LOC after pre-existing pathology extraction but still owns detailed open/status/tool behavior.
- **DV-AUD-D-001 — OPEN:** cache/store paints egui directly through `TileStore::draw_ready_tile`.
- **DV-AUD-D-002 — OPEN:** uploader constructs `ViewerOpenOptions` and reads environment-backed configuration.
- **DV-AUD-D-003 — PARTIALLY FIXED:** annotation/workspace responsibilities have pre-existing splits, but `workspace.rs` is 2,089 LOC and still mixes editor state, promotion, payload ownership, and coordination.
- **DV-AUD-D-004 — PARTIALLY FIXED:** root app delegates more pathology behavior but still owns detailed interaction/status logic.
- **DV-AUD-D-005 — OPEN:** core model still combines process configuration, cache budgets, identifiers, summaries, tile/color data, and errors.
- **DV-AUD-D-006 — OPEN:** telemetry update methods call `eprintln!` directly.
- **DV-AUD-D-007 — OPEN:** canvas owns planning, warming, scheduler publication, cache protection, upload polling, and drawing.

### Correctness and reliability

- **DV-BUG-001 — OPEN:** loader construction records spawn failures but remains constructible and admits work even if all workers fail.
- **DV-BUG-002 — OPEN:** failed-tile retry is represented by state/booleans rather than a typed retry policy.
- **DV-BUG-003 — ALREADY FIXED:** `TileCoverage` separates ready/pending/failed/missing; failed tiles remain uncovered. Existing tests: `failed_tile_stops_loading_but_remains_uncovered`, `failed_target_draws_no_placeholder_over_the_coarser_fallback`.
- **DV-BUG-004 — PARTIALLY FIXED:** CPU retry state is cleared when a queued retry is pruned, but it is not typed by generation/failure class/attempt.
- **DV-BUG-005 — OPEN:** CPU fallback read mode is retained, but visible-priority restoration is not represented or proven as a retry invariant.
- **DV-BUG-006 — PARTIALLY FIXED:** failed batches receive individual recovery, but failure types are string-based and need model tests after ownership migration.
- **DV-BUG-007 — OPEN:** asynchronous wgpu validation is not mapped to a tile-level typed outcome.
- **DV-BUG-008 — OPEN:** device-loss recovery is explicitly listed as a known compromise.
- **DV-BUG-009 — OPEN:** GPU out-of-memory is not an explicit upload error variant.
- **DV-BUG-010 — PARTIALLY FIXED:** partial/missing/surplus upload outcomes have accounting tests, but ownership uses remove/reinsert state transitions rather than an RAII transaction.
- **DV-BUG-011 — OPEN:** open workers are not cooperatively cancellable.
- **DV-BUG-012 — PARTIALLY FIXED:** open jobs are capped at two with latest-pending replacement, but running work is detached from cancellation and not joined.
- **DV-BUG-013 — OPEN:** slide/UI conversion still narrows coordinates to `f32` before the final presentation boundary.
- **DV-BUG-014 — OPEN:** geometry tolerance policy has not been independently re-audited and centralized.
- **DV-BUG-015 — OPEN:** skipped-leading-level canonical geometry needs a regression test.
- **DV-BUG-016 — PARTIALLY FIXED:** dense frame expectation multiplies tile grid by declared optical paths and focal planes, but does not parse the actual dimension organization or per-frame groups.
- **DV-BUG-017 — OPEN:** concatenation aggregation uses per-instance counts and does not establish multi-instance dimension membership.
- **DV-BUG-018 — REGRESSED:** root app status strings include full `path.display()` values; ordinary warnings/errors may expose local paths.
- **DV-BUG-019 — PARTIALLY FIXED:** CUDA has a checked host-download route and parity workflow, but there is no local CUDA runtime validation on this macOS host.
- **DV-BUG-020 — PARTIALLY FIXED:** CPU and Metal ICC paths and LUT proof/cache tests exist; current dirty tree must compile before they can be rerun.
- **DV-BUG-021 — PARTIALLY FIXED:** CPU fallback diagnostics exist, but failure classes are strings and malformed device-path coverage is incomplete.
- **DV-BUG-022 — PARTIALLY FIXED:** queue/in-flight cancellation exists; running kernels and open/warmer work can still consume obsolete capacity.
- **DV-BUG-023 — OPEN:** result acceptance depends on `active_demand_keys`, `accepted_result_keys`, poll `relevant_tiles`, loader batch currency, and generation checks.
- **DV-BUG-024 — OPEN:** poisoned loader/warmer mutexes recover access without proving semantic state validity.
- **DV-BUG-025 — OPEN:** cache transitions and accounting depend on remove/reinsert mutation.
- **DV-BUG-026 — OPEN:** configuration is read independently in main/core/canvas/loader/uploader/stats.
- **DV-BUG-027 — OPEN:** counters are not yet proven to correspond to unique typed tile events.

### Performance findings

All performance items are hypotheses until `PERFORMANCE.md` contains measured evidence.

- **DV-PERF-001 — OPEN:** frame planning rebuilds vectors.
- **DV-PERF-002 — OPEN:** frame planning sorts tile vectors.
- **DV-PERF-003 — OPEN:** frame planning clones/constructs hash sets.
- **DV-PERF-004 — OPEN:** one frame creates `FrameTileDemand`, `TilePollRequest`, multiple lane vectors, and key sets.
- **DV-PERF-005 — OPEN:** eviction repeatedly searches map candidates; benchmark required.
- **DV-PERF-006 — OPEN:** cache profiles are fixed.
- **DV-PERF-007 — ALREADY FIXED:** uploader documents/tests that empty and CPU-only batches do not create/submit a compute encoder; rerun after baseline repair.
- **DV-PERF-008 — OPEN:** Metal per-tile resource creation remains.
- **DV-PERF-009 — OPEN:** uniform/bind-group cost unmeasured.
- **DV-PERF-010 — PARTIALLY FIXED:** bounded LUT texture LRU exists; binding reuse cost remains unmeasured.
- **DV-PERF-011 — OPEN:** stale decode/level preparation can consume capacity.
- **DV-PERF-012 — OPEN:** process-wide CPU budget does not exist.
- **DV-PERF-013 — OPEN:** JP2K inner/outer concurrency can multiply.
- **DV-PERF-014 — OPEN:** compressed payload copy chain unmeasured.
- **DV-PERF-015 — OPEN:** device-side RGB8 expansion opportunity unmeasured.
- **DV-PERF-016 — OPEN:** memory calculations are duplicated and peak scope is incomplete.
- **DV-PERF-017 — OPEN:** loader heap compaction cost unmeasured.
- **DV-PERF-018 — OPEN:** repeated level searches need measurement/classification.
- **DV-PERF-019 — PARTIALLY FIXED:** disabled telemetry avoids some timing/sample work; it is not sink-separated or benchmarked.
- **DV-PERF-020 — OPEN:** handwritten JSON allocates intermediate strings.
- **DV-PERF-021 — OPEN:** DICOM index rebuild/reuse requires fixture telemetry.
- **DV-PERF-022 — OPEN:** warmer and visible scheduler do not share a process budget.
- **DV-PERF-023 — OPEN:** lane policy requires first-sharp/full-coverage A/B data.
- **DV-PERF-024 — OPEN:** decode/upload batch sizes are fixed or environment-selected without retained local evidence.

## P1 — shared checked types and centralized configuration

- **DV-P1-001 — OPEN:** canonical f64 slide geometry.
- **DV-P1-002 — OPEN:** one `ViewportTransform`.
- **DV-P1-003 — DONE:** `tile/memory.rs` is the checked owner for actual edge dimensions, decoded source bytes, CPU RGBA bytes, final texture bytes, temporary conversion bytes, upload peak, and in-flight reservation. Loader admission, store accounting/reconciliation, uploader validation, overview planning, and fallback estimates use it. Tests: `tile_footprint_tracks_edge_dimensions_and_every_memory_component`, `tile_footprint_rejects_zero_dimensions_out_of_range_tiles_and_overflow`, existing tile/store/upload/canvas suites. Commands: narrow red/green test, 125 tile tests, 15 canvas tests, workspace tests, clippy, release build.
- **DV-P1-004 — OPEN:** lane-indexed representation.
- **DV-P1-005 — OPEN:** immutable `DemandSnapshot`.
- **DV-P1-006 — OPEN:** shared bounded `DiagnosticCapture`.
- **DV-P1-007 — OPEN:** startup-parsed `ViewerConfig`.
- **DV-P1-008 — OPEN:** remove environment parsing from ordinary internal open flow.
- **DV-P1-009 — IN PROGRESS:** footprint edge/zero/out-of-range/overflow boundaries are covered; geometry, lane-map, demand, and config boundaries remain.

P1 gate: **OPEN**.

## P2 — typed telemetry and sinks

- **DV-P2-001 — OPEN:** serde record types.
- **DV-P2-002 — OPEN:** preserve/version schema.
- **DV-P2-003 — OPEN:** one counter/sample representation.
- **DV-P2-004 — OPEN:** separate collection, aggregation, record, serialization, sink, overlay.
- **DV-P2-005 — OPEN:** `TelemetrySink`.
- **DV-P2-006 — PARTIALLY FIXED:** deleted the CPU upload `.map(register)` wrapper and its implementation-only test; the broader dead-contract audit remains open.
- **DV-P2-007 — OPEN:** disabled fast path.
- **DV-P2-008 — OPEN:** golden schema and semantic counter tests.

P2 gate: **OPEN**.

## P3 — presentation/cache separation

- **DV-P3-001 — OPEN:** expose read-only ready-tile view; remove drawing from store.
- **DV-P3-002 — OPEN:** presenter/canvas painter.
- **DV-P3-003 — OPEN:** remove egui/viewport dependencies from cache.
- **DV-P3-004 — OPEN:** explicit cache touch separate from painting.
- **DV-P3-005 — OPEN:** retrieval/rect/layer-order tests.

P3 gate: **OPEN**.

## P4 — startup, tasks, and failure lifecycle

- **DV-P4-001 — OPEN:** loader startup result/unavailable state.
- **DV-P4-002 — OPEN:** injectable all-workers-fail regression.
- **DV-P4-003 — OPEN:** cooperative open cancellation.
- **DV-P4-004 — OPEN:** reap open handles without UI blocking.
- **DV-P4-005 — OPEN:** typed failure/retry classes.
- **DV-P4-006 — OPEN:** exact retry transitions/limits.
- **DV-P4-007 — OPEN:** visible CPU fallback priority.
- **DV-P4-008 — ALREADY FIXED:** visual coverage and failed/pending/missing are separate; retain tests through migration.
- **DV-P4-009 — IN PROGRESS:** several named tests exist; startup/open/typed-retry gaps remain.

P4 gate: **OPEN**.

## P5 — authoritative tile state ownership

- **DV-P5-001 — OPEN:** explicit pipeline event/value types.
- **DV-P5-002 — OPEN:** remove queued/decoding from cache.
- **DV-P5-003 — OPEN:** upload transaction ownership.
- **DV-P5-004 — OPEN:** coordinator-exclusive transitions.
- **DV-P5-005 — OPEN:** RAII upload reservation and decoded ownership.
- **DV-P5-006 — OPEN:** replace overlapping key sets.
- **DV-P5-007 — OPEN:** stale rejection before mutation.
- **DV-P5-008 — OPEN:** named-transition byte ledger.
- **DV-P5-009 — OPEN:** transition/model tests.
- **DV-P5-010 — OPEN:** debug assertions for ownership/ledger invariants.

P5 gate: **OPEN**.

## P6 — scheduler, worker pool, decode split

- **DV-P6-001 — PARTIALLY FIXED:** queue policy was mechanically extracted in the dirty tree; ownership still overlaps cache/coordinator.
- **DV-P6-002 — PARTIALLY FIXED:** worker spawn/run functions were extracted; lifecycle owner and startup errors remain in loader.
- **DV-P6-003 — PARTIALLY FIXED:** decode/recovery functions were extracted; failure classes remain strings.
- **DV-P6-004 — OPEN:** scheduler still transports CUDA-related read modes/results indirectly.
- **DV-P6-005 — PARTIALLY FIXED:** decode module does not select lanes, but shared batch types retain policy fields.
- **DV-P6-006 — PARTIALLY FIXED:** worker module is small but operates shared loader state.
- **DV-P6-007 — OPEN:** measure heap compaction before optimization.
- **DV-P6-008 — ALREADY FIXED:** deterministic batch order has existing tests; rerun after baseline repair.
- **DV-P6-009 — IN PROGRESS:** focused queue/decode/worker tests exist; spawn-failure injection/join coverage remains.

P6 gate: **OPEN**.

## P7 — demand, lane policy, frame planning

- **DV-P7-001 — OPEN:** replace both demand structs with `DemandSnapshot`.
- **DV-P7-002 — OPEN:** one `TilePipelinePolicy`.
- **DV-P7-003 — OPEN:** name execution/retention/upload orderings.
- **DV-P7-004 — PARTIALLY FIXED:** `TileFramePlan::build` is mostly pure but tied to egui/f32 types and canvas-private policy.
- **DV-P7-005 — PARTIALLY FIXED:** frame plan has most target fields but not one authoritative demand/output contract.
- **DV-P7-006 — OPEN:** split canvas responsibilities.
- **DV-P7-007 — OPEN:** cache stationary plans.
- **DV-P7-008 — OPEN:** reuse safe scratch buffers after measurement.
- **DV-P7-009 — OPEN:** indexed level lookup where justified.
- **DV-P7-010 — IN PROGRESS:** broad planning tests exist; pan/reversal/resize/determinism gaps require audit.

P7 gate: **OPEN**.

## P8 — cache, ledger, eviction

- **DV-P8-001 — OPEN:** separate decoded/ready/failure/ledger/eviction responsibilities.
- **DV-P8-002 — OPEN:** indexed bounded eviction if benchmark confirms.
- **DV-P8-003 — OPEN:** prevent repeated full scans.
- **DV-P8-004 — OPEN:** one memory ledger.
- **DV-P8-005 — ALREADY FIXED:** oversized entries become terminal failures under the hard ceiling; preserve semantics in the new ledger.
- **DV-P8-006 — OPEN:** explicit safe adaptive budget policy.
- **DV-P8-007 — IN PROGRESS:** many accounting/eviction tests exist; device-loss invalidation remains.
- **DV-P8-008 — OPEN:** 100/1,000/10,000-entry cache benchmark.

P8 gate: **OPEN**.

## P9 — upload and wgpu/Metal errors

- **DV-P9-001 — OPEN:** move WGSL to owned shader source.
- **DV-P9-002 — OPEN:** split CPU and Metal upload.
- **DV-P9-003 — OPEN:** split egui registration from preparation.
- **DV-P9-004 — ALREADY FIXED:** retain `RegisteredTileTexture` RAII.
- **DV-P9-005 — OPEN:** remove uploader open-options/environment ownership.
- **DV-P9-006 — OPEN:** complete typed upload error variants.
- **DV-P9-007 — OPEN:** async wgpu error scope handling.
- **DV-P9-008 — OPEN:** device-loss recovery.
- **DV-P9-009 — ALREADY FIXED:** no encoder submission for empty work; rerun tests.
- **DV-P9-010 — ALREADY FIXED:** CPU-only path avoids Metal compute submission; rerun tests.
- **DV-P9-011 — OPEN:** profile and evaluate resource reuse.
- **DV-P9-012 — PARTIALLY FIXED:** missing/surplus outcomes are reconciled, but result types do not structurally enforce one-to-one cardinality.
- **DV-P9-013 — ALREADY FIXED:** partial/missing/surplus upload tests exist; preserve through transaction refactor.

P9 gate: **OPEN**.

## P10 — level warmer/background workers

- **DV-P10-001 — OPEN:** extract only shared lifecycle/diagnostic mechanics.
- **DV-P10-002 — OPEN:** shared process source/CPU budget.
- **DV-P10-003 — OPEN:** prove visible admission priority over warming.
- **DV-P10-004 — OPEN:** cancellation prevents stale warmer publication.
- **DV-P10-005 — DONE:** the reviewed test-only prefetch-lane and encoder-prediction wrappers were deleted; production frame-plan and submission-count tests preserve the behavior, and the locked workspace test suite passes.
- **DV-P10-006 — BLOCKED:** real-fixture warming benchmark unavailable.

P10 gate: **OPEN**.

## P11 — geometry, camera, measurement, annotation

- **DV-P11-001 — OPEN:** f64 canonical camera/slide geometry.
- **DV-P11-002 — OPEN:** f32 only at egui/wgpu edge.
- **DV-P11-003 — OPEN:** one pan/zoom implementation.
- **DV-P11-004 — PARTIALLY FIXED:** pre-existing workspace modules split some responsibilities; the 2,089-line runtime remains overloaded.
- **DV-P11-005 — OPEN:** robust scale-aware predicates.
- **DV-P11-006 — OPEN:** coordinate convention contract.
- **DV-P11-007 — OPEN:** deterministic path-minimized export audit.
- **DV-P11-008 — PARTIALLY FIXED:** measurement moved into pathology workspace but controller/output boundary requires audit.
- **DV-P11-009 — PARTIALLY FIXED:** annotation actions/controllers exist; root orchestration boundary requires audit.
- **DV-P11-010 — PARTIALLY FIXED:** app translates some action outcomes; not yet one typed `ToolOutcome`.
- **DV-P11-011 — IN PROGRESS:** geometry tests exist in core/workspace; extreme-offset/tolerance matrix remains.

P11 gate: **OPEN**.

## P12 — core model, DICOM inspection, PHI

- **DV-P12-001 — PARTIALLY FIXED:** annotations/statistics have modules; core `model.rs` remains mixed. Preserve re-exports.
- **DV-P12-002 — OPEN:** process environment remains in core.
- **DV-P12-003 — PARTIALLY FIXED:** inspection already has `dataset` and `dicom` submodules; aggregation remains monolithic.
- **DV-P12-004 — OPEN:** derive TILED_FULL counts from actual multidimensional organization.
- **DV-P12-005 — OPEN:** multidimensional/concatenation/sparse/inconsistent tests.
- **DV-P12-006 — ALREADY FIXED:** bounded metadata preflight exists; retain it.
- **DV-P12-007 — OPEN:** skipped leading level geometry audit.
- **DV-P12-008 — OPEN:** skipped-leading-level regression.
- **DV-P12-009 — REGRESSED:** full local paths appear in ordinary app status and some errors/warnings.

P12 gate: **OPEN**.

## P13 — central CPU execution budget

- **DV-P13-001 — OPEN:** concurrency inventory.
- **DV-P13-002 — OPEN:** determine JP2K thread scope from current upstream API.
- **DV-P13-003 — OPEN:** one execution-budget type.
- **DV-P13-004 — OPEN:** bound multiplication.
- **DV-P13-005 — OPEN:** explicit benchmark overrides.
- **DV-P13-006 — BLOCKED:** CUDA/runtime and real-fixture thread matrix unavailable locally; CPU/Metal subset can proceed later.
- **DV-P13-007 — OPEN:** choose defaults only from retained evidence.

P13 gate: **OPEN**.

## PERF — measured experiments

- **DV-PERF-EXP-001 — OPEN:** frame-plan caching.
- **DV-PERF-EXP-002 — OPEN:** allocation reduction.
- **DV-PERF-EXP-003 — OPEN:** indexed eviction.
- **DV-PERF-EXP-004 — OPEN:** dynamic memory policy.
- **DV-PERF-EXP-005 — BLOCKED:** decoder batch-size matrix needs fixture.
- **DV-PERF-EXP-006 — BLOCKED:** queue policy A/B needs fixture and UI telemetry.
- **DV-PERF-EXP-007 — BLOCKED:** stale-work cancellation measurement needs fixture.
- **DV-PERF-EXP-008 — OPEN:** CPU upload instrumentation/parity.
- **DV-PERF-EXP-009 — BLOCKED:** Metal upload profiling needs suitable fixture/run.
- **DV-PERF-EXP-010 — OPEN:** GPU submission batching instrumentation.
- **DV-PERF-EXP-011 — BLOCKED:** warming A/B needs fixture.
- **DV-PERF-EXP-012 — BLOCKED:** DICOM index reuse needs DICOM WSI fixture.
- **DV-PERF-EXP-013 — BLOCKED:** compressed-copy chain requires upstream/runtime profiling.
- **DV-PERF-EXP-014 — OPEN:** level lookup/indexing.
- **DV-PERF-EXP-015 — OPEN:** telemetry overhead.
- **DV-PERF-EXP-016 — BLOCKED:** CUDA runtime experiment unavailable on macOS.
- **DV-PERF-EXP-017 — BLOCKED:** external frame-time capture not yet configured.

PERF gate: **OPEN**.

## P14 — final cleanup

- **DV-P14-001 — OPEN:** root app becomes orchestration only.
- **DV-P14-002 — PARTIALLY FIXED:** the CPU upload pass-through wrapper was deleted and duplicate same-file target detection was consolidated; the final whole-tree audit remains open.
- **DV-P14-003 — OPEN:** classify ownership copies/allocations.
- **DV-P14-004 — OPEN:** comment audit.
- **DV-P14-005 — OPEN:** final dependency-direction audit.
- **DV-P14-006 — OPEN:** public API/environment audit.
- **DV-P14-007 — PARTIALLY FIXED:** three tests coupled only to deleted micro-wrappers were removed while production behavior tests were retained; the final test audit remains open.
- **DV-P14-008 — OPEN:** remove transitional adapters with deletion gates.

P14 gate: **OPEN**.

## Final independent re-audit and completion

- **DV-FINAL-001 — OPEN:** independently re-audit all 25 categories from the assignment without relying on this plan's status marks.
- **DV-FINAL-002 — OPEN:** run the exact valid local quality matrix and record unavailable hardware/platform gates.
- **DV-FINAL-003 — OPEN:** execute all available manual workflows in `MANUAL_ACCEPTANCE.md`.
- **DV-FINAL-004 — OPEN:** verify every definition-of-done item with code, tests, commands, and measurements.
- **DV-FINAL-005 — OPEN:** produce the required precise final implementation report and state that no remote mutation occurred.
