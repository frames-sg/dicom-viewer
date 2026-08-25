# Refactor Status

- **Audit anchor:** `87aa73d8cf5223ef6ba674e2ef7747453df06442`
- **Current HEAD:** `87aa73d8cf5223ef6ba674e2ef7747453df06442`
- **Current branch:** `main` (`main...origin/main`)
- **Working-tree state:** dirty before this plan and still dirty; all pre-existing pathology/workspace/supply-chain/loader-split changes are preserved. The latest checkpoint closes the sibling annotation API migration without changing the active tile-refactor scope.
- **Active phase:** P1 shared checked types and centralized configuration
- **Active task ID:** `DV-P1-004`
- **Exact viewer files modified by this checkpoint:** `crates/dicom-viewer-core/src/{annotations/mod.rs,annotations/workspace/export.rs,annotations/workspace/compatibility.rs,lib.rs,workspace_export_tests.rs}`; `apps/dicom-viewer/src/app/{annotation_actions.rs,workspace.rs,workspace/tests.rs,workspace_actions.rs,raster/io.rs,raster/tests.rs}`; `apps/dicom-viewer/src/bin/annotation_probe/{convert_geojson.rs,convert_raster.rs}`; `docs/DICOM_NATIVE_CONVERSION.md`; `docs/refactor/{MASTER_PLAN.md,STATUS.md}`. The sibling annotations crate additionally changed its pathology document bundle, tests, and durable plan.
- **Current invariant being established:** `DV-INV-027`: every tile-memory consumer uses one checked footprint for edge dimensions, decoded/texture/temporary/peak/reservation bytes; no caller hand-computes RGBA bytes.
- **Completed task IDs since last checkpoint:** `DV-G0-001`, `DV-G0-002`, `DV-G0-003`, `DV-G0-004`, `DV-G0-006`, `DV-G0-007`, `DV-G0-011`, `DV-P1-003`, `DV-P10-005`, `DV-DUP-008` through `DV-DUP-012`
- **Tests currently green:** `git diff --check`; formatting; locked workspace clippy with warnings denied; locked workspace tests with 346 passed and 2 documented ignores; focused viewer tests with 234 passed and 1 documented ignore. Earlier checkpoint evidence for the release build, cargo machete, cargo audit, and cargo deny remains valid.
- **Tests currently failing and why:** none in the executed local matrix. The two ignored tests require a local WSI fixture or an optimized manual pathology characterization.
- **Current backend under test:** CPU and macOS/Metal compile surface; no runtime WSI fixture. CUDA runtime unavailable on this host.
- **Current benchmark fixture:** none configured. Repository tests generate synthetic DICOM/HTJ2K fixtures; `DICOM_VIEWER_WSI_FIXTURE` is unset.
- **Last measured result:** no viewer performance claim. The post-migration release build completed in 2m29s; this is build timing only. No real fixture is configured.
- **Next three function-level actions:** (1) add red tests for exhaustive `QueueLane` iteration/indexing and a fixed `LaneMap<T>`; (2) replace repeated visible/transition/fallback/overview/prefetch counter fields in the narrowest scheduler-stat boundary without changing JSON schema; (3) rerun lane/telemetry goldens and workspace clippy/tests before expanding the migration.
- **Known blockers:** real WSI performance fixture absent; CUDA runtime unavailable; Windows/Linux runtime unavailable; manual UI acceptance not yet run.
- **Decisions that must survive compaction:** preserve all pre-existing dirty changes; do not reset/clean/commit/publish; use `/Users/user/Bench/frames/dicom-viewer` as the single LSP root; keep unsafe Metal code inside `crates/metal-wgpu-interop`; `AllowLoss` preserves the former read-only SEG overlay projection and every blocking diagnostic code is surfaced in UI status; all newly created ANN/SEG/SR/PM objects use `frames_viewer_producer` while imported rewrites retain imported identity; `TileFootprint` is the only tile-memory arithmetic owner; `eframe/persistence` remains required because `RevisionStore::for_application` calls its gated `eframe::storage_dir`; do not credit pre-existing loader extraction as newly implemented work.
- **Last `git status --short`:** 36 tracked paths appear modified/deleted relative to the anchor and 92 individual files are untracked; all pre-existing entries remain preserved.
- **Last `git diff --stat`:** 36 tracked files changed, 3,002 insertions, 3,591 deletions. Untracked files, including the annotation/workspace modules and durable documents, are not included.
- **Date/time of update:** 2026-08-21 23:48:39 EDT

## Active phase reread checklist

Before the next production edit, reread `MASTER_PLAN.md`, this file,
`INVARIANTS.md`, and `TILE_STATE_MODEL.md`, then rerun the narrow failing command.
