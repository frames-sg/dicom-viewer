# Research release checklist

This project may be released only as a research-use-only viewer for inputs
that contain no patient data. It has no clinical, diagnostic, or
de-identification claim.

## Automated gates

Run from a clean checkout with the locked dependency graph:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo check --workspace --all-targets --locked --features cuda
cargo build --workspace --release --locked
cargo machete
cargo audit --deny unsound
cargo deny check advisories bans licenses sources
```

All three desktop CI targets must pass. The macOS Metal tests and a CUDA-host
runtime validation must pass when those backends are included in a release.

## Reproducible source gate

The locked graph must resolve exact `wsi-rs` 0.5.2 and J2K 0.8.0 releases from
crates.io, including registry checksums in `Cargo.lock`. CI and packaging must
build without sibling codec checkouts or local-only source overrides. Run
`cargo metadata --locked --format-version 1` from a clean checkout and confirm
that it leaves `Cargo.lock` unchanged.

## Interactive performance gate

Do not infer end-to-end frame pacing from `app_ui_cpu_ms` or `tile_probe`.
Before describing a build as real-time or interactively responsive:

1. Record the release commit, Rust version, operating system, CPU, GPU, memory,
   display resolution and refresh rate, power mode, and SHA-256 checksums for
   every acceptance fixture.
2. Predeclare numeric acceptance limits for end-to-end frame intervals, the
   longest allowed stall, interaction-to-first-sharp latency, full-target
   coverage latency, peak resident memory, and decode or upload errors.
3. Run the optimized viewer with `DICOM_VIEWER_DEBUG_STATS=1`, retain its JSONL
   stderr, and externally capture presentation frame times. Exercise sustained
   pan and zoom plus repeated fit, level transitions, facts pagination,
   measurement, annotation, and GeoJSON replacement.
4. Test representative SVS, DICOM VL WSI, and raw JPEG 2000 research inputs for
   at least 30 minutes in total. Include cold starts, warm revisits, malformed
   input, rapid direction reversals, and memory pressure.
5. Report p50, p95, p99, and maximum observations alongside every predeclared
   limit. A missing format, backend, metric, or required runtime is a failed
   gate, not a pass.

## Acceptance and packaging

- Open representative SVS, DICOM VL WSI, and raw JPEG 2000 research fixtures;
  exercise fit, pan, zoom, facts pagination, measurement, annotation, and
  atomic GeoJSON replacement.
- Exercise malformed metadata, oversized geometry, and decoder failures and
  confirm the viewer reports errors without terminating.
- Build packages on their native operating systems. Sign and notarize the
  macOS application, Authenticode-sign the Windows package, and publish
  SHA-256 checksums and an SBOM with every artifact.
- Install each packaged artifact on a clean machine and repeat the critical
  open, render, and GeoJSON export flow.

Do not describe a release as production-ready or real-time while the
reproducible source, interactive performance, native packaging, signing, or
representative acceptance gates remain open.
