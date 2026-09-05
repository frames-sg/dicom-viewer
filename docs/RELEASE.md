# Release checklist

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

The locked graph must resolve `wsi-rs` 0.6.0 at revision `b940ea94`, J2K 0.10.0
at revision `57b6af89`, and `wsi-dicom-annotations` 0.1.2 at revision `71851b4a`
from their upstream Git repositories. The annotation revision includes the shared
`metadata::open_metadata_object` API; the manifest pins its full 40-character SHA.

CI and packaging must build without sibling checkouts or local source overrides.
Run `cargo metadata --locked --format-version 1` from a clean checkout and confirm
that it leaves `Cargo.lock` unchanged. Moving annotations to crates.io requires a
matching owner publication and a reviewed registry lockfile refresh.

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
4. Test representative SVS, DICOM VL WSI, and raw JPEG 2000 inputs for
   at least 30 minutes in total. Include cold starts, warm revisits, malformed
   input, rapid direction reversals, and memory pressure.
5. Report p50, p95, p99, and maximum observations alongside every predeclared
   limit. A missing format, backend, metric, or required runtime is a failed
   gate, not a pass.

## Acceptance and packaging

- Include [third-party notices](../THIRD_PARTY_NOTICES.md) and the MPL 2.0
  license text with Windows artifacts, retaining access to the corresponding
  `dwrote` source described in that notice.

- Open representative SVS, DICOM VL WSI, and raw JPEG 2000 fixtures;
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
