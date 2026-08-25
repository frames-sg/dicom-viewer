# Tile State Model

## Current ownership model

```text
SlideCanvas.paint
  builds TileFramePlan
  builds cache_relevant/pinned/overview key sets
  builds FrameTileDemand ------------------------------+
  builds TilePollRequest --------------------------+   |
                                                   |   v
TileRenderer                                      | canonicalize/cap demand
  owns demand_epoch                               | active_demand_keys
  owns active_demand_keys                         | accepted_result_keys
  owns accepted_result_keys                       | loader batch currency
  delegates cache state and upload                |
       |                                           |
       v                                           |
TileStore                                          |
  missing -> Queued -> Decoding -> Decoded -> Uploading -> Ready
                          |            |          |
                          +----------> Failed <---+
  owns resident/pinned bytes, eviction, retry booleans,
  coverage, texture registration lifetime, and egui drawing

TileLoader
  independently owns queued heap/canonical entries, in-flight batches,
  cancellation tokens, worker availability, decoded reservations, results
       |
       v
decode module -> ViewerStudy/wsi-rs source reads -> per-tile recovery/results
```

The same tile therefore has scheduler states in both loader and store. Acceptance uses
several overlapping key sets and identities. Upload ownership is temporarily represented
as a long-lived store state and reconciled through remove/reinsert mutation.

## Target ownership model

```text
Viewport + study summary + policy + read-only pipeline snapshot
                         |
                         v
                 TileFramePlan (pure)
                         |
                         v
                 DemandSnapshot (immutable)
                         |
                         v
TileCoordinator --------------------------------------------------+
  sole transition owner                                           |
  validates generation/source/demand identity                     |
  exposes read-only FrameTileStatus                                |
     | admission/results           | cache commit                  | upload job/outcome
     v                             v                               v
TileScheduler                 TileCache                      TileUploader
  Queued                        Decoded                        CPU prepare/write
  InFlight                      Ready                          Metal import/convert
  cancellation                  Failed                         wgpu validation
  priority/fairness             MemoryLedger                  texture registration
  batch/worker capacity         bounded eviction              typed device errors
     |                             ^                               |
     v                             |                               |
DecodeWorker ---------------- DecodeBatchResult ------------------+
  source read, order/cardinality, recovery, cancellation, diagnostics

TilePresenter
  reads ReadyTileView + ViewportTransform
  owns rects, clipping, paint calls, target/fallback composition
  never mutates lifecycle or accounting (explicit cache touch is separate)

Telemetry
  observes typed events/snapshots -> collector -> record -> sink/overlay
  cannot change scheduling behavior
```

## Authoritative owners

| State/resource | Sole owner | Allowed operations |
| --- | --- | --- |
| Missing | coordinator-derived absence | admit or report missing |
| Queued | scheduler | deduplicate, reprioritize, cancel, dispatch |
| InFlight | scheduler | hold batch identity, cancellation, decoded-byte permit, worker slot |
| Decode execution | decode worker | source call, cardinality/order validation, per-tile recovery, diagnostics |
| Decoded | cache, committed only by coordinator | retain/evict, lend to upload transaction |
| Uploading | short-lived `UploadTransaction`, coordinated by coordinator | own decoded tile and peak reservation; commit/rollback exactly once |
| Ready | ready cache | texture lifetime, byte ledger, read-only view, explicit touch |
| Failed | failure cache/coordinator domain | typed class/attempt/terminal metadata; never visual coverage |
| Presentation | presenter | read ready views and paint; no lifecycle mutation |
| Metrics | telemetry collector | observe immutable events/snapshots only |

## Events and transitions

```text
DemandPublished(snapshot)
  Missing -> Queued                          coordinator asks scheduler to admit
WorkerDispatched(batch)
  Queued -> InFlight                        scheduler only
DecodeSucceeded(result)
  InFlight -> Decoded                       coordinator validates then commits
DecodeCancelled/Obsolete(result)
  InFlight -> Missing/Removed               no cache mutation
DecodeFailed(result)
  InFlight -> Queued                        typed retry admitted by coordinator
  InFlight -> Failed                        terminal/exhausted outcome
UploadStarted(job)
  Decoded -> UploadTransaction              transaction owns input + reservation
UploadSucceeded(outcome)
  UploadTransaction -> Ready                atomic commit
UploadRetryableFailure(outcome)
  UploadTransaction -> Decoded              rollback, preserving bytes/input
UploadCpuFallback(outcome)
  UploadTransaction -> Queued               explicit CPU route, visible priority preserved
UploadTerminalFailure(outcome)
  UploadTransaction -> Failed               reservation/input reconciled once
DemandSuperseded(snapshot)
  obsolete Queued/InFlight -> cancelled     scheduler
  stale results -> rejected                 coordinator before cache mutation
DeviceLost
  stop GPU admission; invalidate Ready GPU textures; preserve eligible Decoded data;
  recreate resources or enter explicit CPU/unavailable state; suppress per-tile storms
```

## Cancellation and acceptance

- Demand identity comprises source/study generation, demand epoch/fingerprint, and the
  immutable lane-indexed keys/protection set.
- Scheduler cancellation stops queued work immediately and signals in-flight batches.
- Decode checks cancellation before source work, between recoverable elements, after
  source return, and before publication. Running codec kernels may remain non-preemptive.
- Coordinator rejects obsolete source, generation, demand, batch, or key identities before
  any cache transition or byte-ledger mutation.
- Cancelled work is not a failure and does not consume retry count.

## Retry and fallback

- Retry identity includes generation, original route, typed failure class, and attempt.
- Batch failure is isolated into ordered individual attempts only when the decoder supports it.
- CUDA download is explicit; at most one checked CPU retry is permitted.
- Metal import/validation/device failure either rolls back decoded ownership or admits a
  typed CPU retry. A visible tile retains visible priority.
- Permanent corrupt/unsupported input becomes `Failed`; it does not count as visual coverage.

## Memory ownership

- `TileFootprint` is the checked value source for edge dimensions, decoded bytes, texture
  bytes, conversion temporary bytes, upload peak, and in-flight reservation.
- Scheduler owns in-flight decoded reservations.
- Cache ledger owns decoded/ready resident bytes and pinned subset.
- Upload transaction owns its peak reservation and decoded input until commit/rollback.
- Eviction never sees half-transitioned accounting and cannot evict an active transaction.
