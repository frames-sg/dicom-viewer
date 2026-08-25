# Failure Matrix

Every injection point must have an explicit owner, observable outcome, and regression
test. “Telemetry” means a typed event once P2/P5 are complete.

| ID | Injection point | Required behavior / owner | Current evidence | Status |
| --- | --- | --- | --- | --- |
| DV-FAIL-001 | open worker creation | open queue rejects/retains safe pending state; UI receives infrastructure error; no phantom active task | spawn errors surfaced, queue bounded at two | PARTIAL |
| DV-FAIL-002 | source open | typed contextual error, sanitized ordinary path label, no state replacement | generation rejection exists; full path status exists | OPEN |
| DV-FAIL-003 | level preparation | bounded error event, no stale generation publication, visible decode continues | warmer tests cover failures/stale events | PARTIAL |
| DV-FAIL-004 | queue admission | cap/worker-unavailable explicit result; store/cache not marked queued on rejection | cap tests exist; zero-worker construction defect remains | OPEN |
| DV-FAIL-005 | source read | typed transient/permanent/cancelled result with tile identity | string failure + cancellation tests | PARTIAL |
| DV-FAIL-006 | batch cardinality | validate before cache mutation; recover individually where supported | decoder tests cover wrong cardinality/individual recovery | PARTIAL |
| DV-FAIL-007 | cancellation | no failure count, retry, stale cache commit, or subsequent batch element | loader/store cancellation tests exist | PARTIAL |
| DV-FAIL-008 | decoder panic | contain at worker boundary; produce typed infrastructure/tile result; worker pool remains coherent | panic containment tests exist | PARTIAL |
| DV-FAIL-009 | CUDA download | explicit `CudaDownload`; one ordered CPU retry; no hidden readback | decode/core parity tests exist, runtime unavailable locally | PARTIAL |
| DV-FAIL-010 | CPU retry | attempt identity and limit; visible priority retained | read-mode/boolean state and pruning test | OPEN |
| DV-FAIL-011 | Metal import | rollback decoded ownership or typed CPU retry; coherent diagnostic | store/uploader tests for failures | PARTIAL |
| DV-FAIL-012 | wgpu validation | scoped synchronous/async validation maps to upload outcome | incomplete error model | OPEN |
| DV-FAIL-013 | wgpu device loss | stop uploads, invalidate ready device textures, recreate or explicit fallback/unavailable | documented known compromise | OPEN |
| DV-FAIL-014 | wgpu out of memory | explicit non-string device OOM outcome; no retry storm | no explicit variant | OPEN |
| DV-FAIL-015 | texture creation | checked dimensions/limits/bytes; input retained or terminal typed failure | preflight/accounting tests | PARTIAL |
| DV-FAIL-016 | texture registration | unregister partial resources; reconcile one input/outcome | RAII texture wrapper and partial outcome tests | PARTIAL |
| DV-FAIL-017 | upload reconciliation | missing/surplus/partial outcomes cannot strand bytes/input | focused store tests | PARTIAL; replace remove/reinsert with transaction |
| DV-FAIL-018 | cache eviction | protected ordering, hard bound, oversized/pinned policy, deterministic result | broad store tests | PARTIAL; indexed policy/perf open |
| DV-FAIL-019 | DICOM inspection | bounded preflight, contextual sanitized failure, multidimensional facts | strong preflight tests; dimension model incomplete | PARTIAL |
| DV-FAIL-020 | annotation persistence | atomic publication; old data survives failure/cancel; contextual sanitized status | pre-existing workspace tests/docs | PARTIAL; outside tile first slice |
| DV-FAIL-021 | open-job supersession | cooperative cancellation, no stale app update, bounded handles, eventual reap | generation filter/latest pending; no cooperative cancellation/join | OPEN |

## Failure classes

The target typed classes are `Cancelled`, `TransientSource`, `CudaDownload`,
`DeviceLost`, `DeviceOutOfMemory`, `UploadValidation`, `PermanentInvalidInput`,
`PermanentUnsupported`, and `InfrastructureUnavailable`. Exact naming may adapt to
existing repository conventions, but string inspection and ambiguous retry booleans are
not acceptable ownership mechanisms.
