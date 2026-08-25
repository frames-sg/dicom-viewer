# Manual Acceptance

Use research-only fixtures with no patient data. Record fixture SHA-256, build command,
backend, OS/hardware, start/end time, and observed failures. Do not put full local paths
in public telemetry artifacts.

## Common setup

1. Build the exact dirty checkpoint with `cargo build --workspace --release --locked`.
2. Start the viewer with debug telemetry disabled, then repeat the performance workflow
   with `DICOM_VIEWER_DEBUG_STATS=1` when telemetry validation is intended.
3. Record the source label, format, dimensions, tile dimensions, codec, ICC status, and
   backend. Store the full local path only in a private test log if explicitly required.
4. Confirm no panic, unbounded status growth, path leak in JSONL, or silent backend fallback.

## DV-MAN-001 — local file open

- Open a supported single WSI file from the picker and command line.
- Verify metadata arrives, initial fit is correct, overview/fallback appears, target tiles
  sharpen, loading settles, and the ordinary status uses a basename/redacted label.
- Supersede the open with another source and confirm stale results do not replace it.

## DV-MAN-002 — DICOM folder open

- Open a folder containing one coherent VL WSI series with multiple instances.
- Verify bounded enumeration, correct instance/frame facts, and rejection of mixed series.
- Exercise a malformed/oversized candidate and confirm a contextual non-crashing error.

## DV-MAN-003 — zoom

- Perform single-step, continuous, and rapid reversal zoom around visible anchors.
- Verify anchor stability, held-level behavior, visible priority, first-sharp transition,
  no blanking when a coarser ready fallback exists, and stale-work cancellation.

## DV-MAN-004 — pan

- Pan slowly and rapidly at fit, intermediate, and highest useful zoom.
- Verify no coordinate drift, deterministic edge-tile geometry, bounded queue growth, and
  no obsolete results appearing in the current viewport.

## DV-MAN-005 — level transition

- Cross adjacent pyramid thresholds in both directions, stop near hysteresis boundaries,
  and rapidly reverse.
- Verify target/held/fallback ordering, no permanent low-resolution hold, and correct
  full-target coverage semantics when a target tile fails.

## DV-MAN-006 — overview fallback

- Zoom deeply, move away, then fit the slide and revisit a recent region.
- Verify overview reservation is bounded, reusable, subordinate to the hard memory ceiling,
  and never painted over a ready sharper target.

## DV-MAN-007 — measurement

- Create, edit/select, and remove a ruler at normal and extreme zoom.
- Verify base-coordinate stability, physical-unit calculation when spacing exists, clear
  unavailability when it does not, undo/redo, and deterministic export identity.

## DV-MAN-008 — annotation

- Create/select/edit independent polygon and point findings; create separate same-class
  segments with Add/Erase brush/polygon primitives; exercise undo/redo and autosave.
- Test large coordinates, near-collinear edges, touching/repeated points, and an invalid
  self-intersection. Verify no geometry silently changes precision or topology.

## DV-MAN-009 — annotation export

- Export portable workspace, scheme-aware GeoJSON, eligible ANN/SEG/SR, and one supported
  raster/PM flow. Exercise existing-destination decision, cancellation, and injected failure.
- Verify atomic publication, deterministic content where specified, preserved prior output,
  and no unnecessary source path in exported metadata/JSON.

## DV-MAN-010 — CPU mode

- Run with `DICOM_VIEWER_TILE_BACKEND=cpu` using JPEG and JPEG-2000/HTJ2K fixtures.
- Verify pixel/ICC parity, visible priority, bounded memory, no Metal compute conversion,
  and no GPU command submission for empty work.

## DV-MAN-011 — Metal mode (macOS)

- Run `auto` on the exact renderer Metal device with a supported resident J2K source and
  an ICC-profiled source.
- Verify device identity, pitch/edge tiles, LUT proof behavior, one batch submission,
  explicit CPU fallback diagnostics, and pixel parity with forced CPU.

## DV-MAN-012 — CUDA mode (supported Linux only)

- Build/run with `--features cuda` and the runtime-required environment from the CUDA CI.
- Verify actual CUDA selection, checked pitch-aware host download, pixel parity, exactly one
  CPU retry on download failure, and explicit diagnostics. Do not claim zero-copy.

## DV-MAN-013 — device failure/fallback

- Where reproducible, inject Metal import mismatch, wgpu validation, device loss, and OOM.
- Verify uploads stop coherently, ready device resources are invalidated, decoded ownership
  is reconciled, recovery/fallback is explicit, and diagnostics do not storm per tile.

## Acceptance log

No workflow has been executed under this plan yet. Hardware/fixture limitations must be
recorded here rather than converted into a pass.
