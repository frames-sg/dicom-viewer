# Tile Pipeline Invariants

These are testable contracts, not aspirations. A transition or extraction is incomplete
until the relevant invariant has a focused test.

- **DV-INV-001:** a `TileKey` is owned by at most one lifecycle state at a time.
- **DV-INV-002:** `Queued` and `InFlight` exist only in the scheduler.
- **DV-INV-003:** `Decoded`, `Ready`, and `Failed` exist only in the cache/coordinator domain.
- **DV-INV-004:** every state transition has one named owner.
- **DV-INV-005:** every state transition commits completely or leaves the previous state recoverable.
- **DV-INV-006:** resident byte accounting equals authoritative cache entries plus active upload reservations.
- **DV-INV-007:** pinned bytes are a subset of resident bytes unless a separately named in-flight reservation is included.
- **DV-INV-008:** an upload transaction owns its decoded input and peak-memory reservation until commit or rollback.
- **DV-INV-009:** a cancelled or obsolete result cannot populate a current cache generation.
- **DV-INV-010:** demand publication and result acceptance use the same immutable demand-snapshot identity.
- **DV-INV-011:** every failure is one of: retryable source, retryable device, explicit CPU fallback, cancellation, terminal unsupported input, terminal corrupt input, or application infrastructure failure.
- **DV-INV-012:** a failed tile is never counted as visually covered.
- **DV-INV-013:** terminal failure may count as interaction-resolution complete only through a separately named metric.
- **DV-INV-014:** CPU fallback for a visible tile preserves or raises visible execution priority.
- **DV-INV-015:** batch result order and cardinality are validated before cache mutation.
- **DV-INV-016:** one tile failure does not fail unrelated tiles when per-tile recovery is supported.
- **DV-INV-017:** zero available workers prevents queue admission or returns an explicit unavailable error.
- **DV-INV-018:** no hidden device-to-host download occurs.
- **DV-INV-019:** no command encoder is created or submitted for an empty upload batch.
- **DV-INV-020:** CPU-only uploads perform no unnecessary GPU compute submission.
- **DV-INV-021:** slide-space geometry remains `f64` or checked integer until the egui/wgpu boundary.
- **DV-INV-022:** process environment is parsed once into `ViewerConfig` at application startup.
- **DV-INV-023:** one process execution budget bounds outer tile workers and inner codec threads.
- **DV-INV-024:** disabled telemetry retains no sample vectors and emits no output.
- **DV-INV-025:** ordinary UI status and telemetry do not emit full local source paths.

## Additional boundedness and compatibility invariants

- **DV-INV-026:** every queue, result channel, cache, task collection, diagnostic buffer, and temporary decode/upload allocation has a checked bound.
- **DV-INV-027:** checked tile-footprint arithmetic covers actual edge dimensions, decoded bytes, texture bytes, temporary bytes, upload peak, and in-flight reservation; overflow is an explicit error.
- **DV-INV-028:** a result is accepted only when source identity, study generation, demand identity, and tile identity all match the coordinator's current state.
- **DV-INV-029:** queue reprioritization cannot discard a forced CPU read route or retry attempt identity.
- **DV-INV-030:** stopping or replacing a study cancels queued/in-flight work and prevents stale publication before new work consumes its result.
- **DV-INV-031:** poisoned-lock recovery never silently blesses semantically inconsistent scheduler or warmer state.
- **DV-INV-032:** cache access/touch is explicit; presentation never changes lifecycle or byte accounting.
- **DV-INV-033:** DICOM metadata preflight limits remain enforced before eager object parsing.
- **DV-INV-034:** dense WSI frame expectations follow the actual DICOM dimension organization, optical path, focal plane, concatenation, and instance partitioning rather than a two-dimensional tile-grid assumption.
- **DV-INV-035:** the application and core crates remain `forbid(unsafe_code)`; raw Metal/wgpu handles remain contained in `metal-wgpu-interop`.

## Existing characterization evidence to preserve

The current tree contains tests for queue priority/fairness/caps, reprioritization,
demand epoch changes, cancellation, cardinality recovery, CUDA fallback, stale result
rejection, byte accounting, upload reservation and partial outcomes, eviction protection,
failed coverage, frame-level fallback, and first-sharp/full-coverage metrics. Their exact
green status must be re-established after the baseline compile blocker is fixed.
