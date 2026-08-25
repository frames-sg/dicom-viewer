# Duplication Inventory

Status values describe migration, not whether every occurrence is byte-identical.

| ID | Duplicated concept | Current implementations / semantic differences | Intended authoritative owner | Migration status / deletion gate |
| --- | --- | --- | --- | --- |
| DV-DUP-001 | base-to-screen transform | `CameraView`/viewport plus workspace and external overlays convert `Point2` to `f32` before calling transform | `ViewportTransform` over f64 slide geometry | OPEN; delete local conversion formulas after round-trip/large-coordinate tests |
| DV-DUP-002 | screen-to-base transform | camera/input/workspace interaction paths | `ViewportTransform` | OPEN; replace only after input-boundary tests |
| DV-DUP-003 | camera pan variants | camera/view/viewport helpers | camera module using canonical transform | OPEN; preserve animation semantics |
| DV-DUP-004 | zoom-around variants | camera/view/viewport helpers | camera module | OPEN; preserve anchor and clamp semantics |
| DV-DUP-005 | diagnostic capture setup | loader queue and level warmer independently create optional `Arc<Mutex<Vec<DicomIndexDiagnostic>>>` | bounded `DiagnosticCapture` | OPEN; install/extract disabled-path tests first |
| DV-DUP-006 | diagnostic extraction/poison recovery | loader queue and level warmer lock/extract separately | `DiagnosticCapture` | OPEN; do not treat poisoned semantic contents as valid |
| DV-DUP-007 | worker lifecycle | loader, warmer, open job, background worker each own variants of spawn/channel/cancel/drop/join/panic containment | concrete small managed-worker primitives only for shared mechanics | PARTIAL; dirty tree has `BackgroundWorker`, but loader/warmer/open semantics remain distinct |
| DV-DUP-008 | RGBA byte calculation | formerly tile decoded cost, planned texture preflight, store reconciliation, upload validation, canvas overview/fallback | `TileFootprint` | DONE; relevant production `width × height × 4` arithmetic now exists only in `tile/memory.rs` |
| DV-DUP-009 | texture byte calculation | formerly `planned_texture_bytes`, `DecodedTile::memory_cost`, upload validation/store accounting | `TileFootprint` | DONE; old `TileMemoryCost` and `planned_texture_bytes` deleted |
| DV-DUP-010 | peak upload bytes | formerly `planned_upload_peak_bytes`, decoded memory cost, store reservation | `TileFootprint` | DONE; wrapper delegates to the checked representation and store consumes its value |
| DV-DUP-011 | edge-tile dimensions | formerly planning/preflight and canvas overview arithmetic | `TileFootprint::for_level_tile` plus validated core layout | DONE for tile-memory consumers; geometry planning remains separately owned |
| DV-DUP-012 | loader reservation bytes | formerly planned peak plus a duplicated full-tile `× 8` fallback | `TileFootprint` | DONE; both actual edge and conservative full-tile fallback use `in_flight_reservation_bytes` |
| DV-DUP-013 | cache byte accounting | `resident_byte_len`, insert/remove accounting, upload reservations | `MemoryLedger` named transitions | OPEN |
| DV-DUP-014 | lane fields | demand structs, poll request, loader stats, gauges, JSON, overlays | fixed `LaneMap<T>` keyed by `QueueLane` | OPEN |
| DV-DUP-015 | DICOM index outcome matching | rolling samples, interactions, lifetime counters, overlay/JSON | typed DICOM outcome counters with `record/merge/delta/snapshot` | OPEN |
| DV-DUP-016 | queue classification/priority | `QueueLane` ordering, retention rank, queue comparison, upload ordering | `TilePipelinePolicy` with named execution/retention/upload order | OPEN |
| DV-DUP-017 | upload wrappers | sink trait, budgeted batch, internal preparation, CPU/Metal branches | `TileUploader` jobs/outcomes with CPU/Metal preparation and registration sub-owners | OPEN |
| DV-DUP-018 | file extension knowledge | picker/core/readme/tests | core input capability table exposed to UI/docs tests | OPEN; verify wsi-rs capabilities before centralizing |
| DV-DUP-019 | configuration parsing | core model/tile output/lib, canvas, loader, stats, uploader, main | startup `ViewerConfig`, explicit sub-config values | OPEN |
| DV-DUP-020 | retry/fallback classification | loader read mode, decode strings, store booleans, upload errors/status | typed failure/retry policy at coordinator/decoder/uploader boundary | OPEN |
| DV-DUP-021 | lifetime counter deltas | repeated telemetry fields/match arms | typed counter `delta/snapshot` | OPEN |
| DV-DUP-022 | JSON serialization | multiple `format!` records and manual optional-number formatting | serde records written by `TelemetrySink` | OPEN; schema-v4 goldens first |
| DV-DUP-023 | level lookup/tile plans | canvas scans and separate overview/fallback/prefetch calculations | pure `TileFramePlan` plus validated level index | OPEN; measure/index without corrupting skipped-level semantics |

Deletion is complete only when all former implementations are removed or intentionally
retained with documented distinct semantics. Wrapping duplicates without deleting their
policy ownership does not close an item.
