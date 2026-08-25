# Refactor Decisions

This file is append-only. Supersede a decision with a later record; do not rewrite
history.

## DV-ADR-001 — Preserve the dirty worktree

- **Date:** 2026-08-20
- **Context:** `HEAD` equals the audit anchor, but the worktree already contains a large pathology/workspace feature set, supply-chain changes, and a loader extraction.
- **Alternatives:** reset to the anchor; copy the repository; overwrite overlapping files; preserve and work incrementally in place.
- **Decision:** preserve all pre-existing modifications and untracked files, edit in place, and distinguish pre-existing evidence from changes made under this plan.
- **Rationale:** resetting or overwriting would destroy user work and violate repository instructions. Duplicate `_new` or `_v2` trees would worsen architecture.
- **Consequences:** baseline failures can come from coordinated dirty sibling changes; each refactor slice must inspect the active diff and minimize overlap. No local commit may be created without explicit authorization.
- **Tests/evidence:** session-start `git status --short --branch`, `git diff --stat`, `BASELINE.md`.

## DV-ADR-002 — Concrete owners, not a generic pipeline framework

- **Date:** 2026-08-20
- **Context:** queue, decode, cache, upload, presentation, and telemetry currently overlap.
- **Alternatives:** generic actors/state-machine framework; broad rewrite; concrete scheduler/decoder/cache/coordinator/uploader/presenter boundaries.
- **Decision:** use concrete domain types and one named owner for each transition. Do not introduce a generic actor, pipeline, or state-machine framework.
- **Rationale:** the defect is duplicated ownership. A framework would add indirection without removing state.
- **Consequences:** some modest local duplication is acceptable when semantics differ; extraction follows characterization and ownership, not file-size targets.
- **Tests/evidence:** transition tests required by `INVARIANTS.md`; target map in `TILE_STATE_MODEL.md`.

## DV-ADR-003 — Keep unsafe Metal interoperability isolated

- **Date:** 2026-08-20
- **Context:** `metal-wgpu-interop` validates device identity, pitch, format, offsets, allocation length, and lifetime using narrow audited unsafe code.
- **Alternatives:** move raw handles into uploader/tile modules; replace the boundary; preserve it.
- **Decision:** preserve the crate and keep application/core `forbid(unsafe_code)`.
- **Rationale:** widening raw-platform access would increase correctness and portability risk without solving lifecycle ownership.
- **Consequences:** upload refactors consume safe interop results only. Metal-specific runtime validation remains a separate gate.
- **Tests/evidence:** unsafe-policy lints and interop tests; `rg -n 'unsafe' apps crates`.

## DV-ADR-004 — Repair the baseline before Phase 1 semantics

- **Date:** 2026-08-20
- **Context:** workspace tests do not compile because the dirty viewer calls an older SEG vectorization API than the dirty path dependency provides.
- **Alternatives:** ignore the failure and modify tile code; modify the sibling dependency; minimally adapt the viewer to the typed current API.
- **Decision:** make the smallest compatible viewer-side update, preserve typed loss policy, rerun the workspace baseline, then start checked tile-footprint work.
- **Rationale:** test-first phase work is not trustworthy on a non-compiling baseline, and the sibling repository is not a write target.
- **Consequences:** the compatibility change is recorded separately from Phase 1 architecture work.
- **Tests/evidence:** failing `cargo test --workspace --all-targets --locked`; subsequent evidence to be appended.

## DV-ADR-005 — First architectural slice is checked tile footprint

- **Date:** 2026-08-20
- **Context:** memory arithmetic is repeated across planned texture preflight, planned upload peak, decoded tile cost, loader reservation, and cache accounting.
- **Alternatives:** start with central config, demand snapshot, telemetry, or lifecycle migration.
- **Decision:** after restoring the baseline, introduce a single checked `TileFootprint` value with edge/overflow tests before changing lifecycle ownership.
- **Rationale:** it is a narrow Phase 1 value boundary, reduces primitive arithmetic duplication, and establishes a ledger input without mixing module movement or state semantics.
- **Consequences:** this first slice must preserve existing byte-budget behavior; lifecycle migration remains separate.
- **Tests/evidence:** red/green tests required for zero dimensions, edge tiles, overflow, decoded/texture/temp/peak/reservation values.

## DV-ADR-006 — Planned and actual footprints share one checked representation

- **Date:** 2026-08-20
- **Context:** pre-decode planning knows edge dimensions but must conservatively reserve an RGBA source plus final texture; decoded Metal work knows its actual retained allocation length and may include pitch. CPU decoded data is already final RGBA.
- **Alternatives:** separate estimate and actual structs; store primitive byte fields; one representation with route-specific constructors.
- **Decision:** use one `TileFootprint` with checked constructors for planned RGBA, actual CPU RGBA, and actual device allocations. `cpu_rgba_bytes` describes an overlapping decoded allocation rather than an additional peak allocation; temporary conversion bytes are explicit and currently zero for both routes.
- **Rationale:** one representation removes repeated arithmetic while retaining the semantic difference between planned reservation and actual retained device allocation.
- **Consequences:** in-flight admission uses `in_flight_reservation_bytes`; cache/upload transactions use actual `decoded_source_bytes` and `peak_upload_bytes`. A future route with a real temporary allocation must add it through the constructor rather than hand-adjusting a caller's total.
- **Tests/evidence:** `tile/memory.rs`; new edge/zero/overflow/component tests; existing decoded/store/reservation/upload/overview/fallback tests; workspace clippy/tests/release build.
