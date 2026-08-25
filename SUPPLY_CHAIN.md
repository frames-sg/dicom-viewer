# Supply-chain policy

`cargo audit --deny unsound` and `cargo deny check advisories bans licenses
sources` are release gates. `deny.toml` is the machine-readable policy.

## Raster conversion dependencies

The production raster boundary adds only these direct crates:

- `tiff = 0.11.3` (MIT, declared Rust 1.85): the converter's direct dependency
  disables defaults and requests only `deflate`, `lzw`, and `zstd`. The desktop
  graph's pre-existing `image`/clipboard path also unifies TIFF's default
  `fax`/`jpeg` features, but conversion still rejects lossy compression before
  decoding it; WebP is not enabled.
- `npyz = 0.9.1` (MIT): no optional features are enabled. The crate publishes
  no `rust-version`, so compatibility is established by the locked workspace
  build on Rust 1.96 rather than an upstream MSRV declaration.
- `zarrs = 0.23.13` (MIT OR Apache-2.0, declared Rust 1.91): defaults are
  disabled; only `filesystem`, `blosc`, `crc32c`, `gzip`, `sharding`, and
  `zstd` are enabled. The application exposes only a local filesystem array
  path and does not compile the `ndarray`, async, remote-store, or optional
  uncommon-codec surfaces.

All three versions exist in the locked registry graph, compile below the
workspace Rust 1.96 floor (or are verified directly there where no MSRV is
declared), and are covered by the release advisory/license/source gates. The
TIFF and NPY crates own their mature file-format decoding; `zarrs` owns the
substantially more complex Zarr v2/v3 metadata, chunk-grid, sharding, and codec
behavior. Reimplementing those parsers locally would enlarge the untrusted
input surface.

## Temporary security patch

`vendor/lru` is the crates.io `lru 0.16.4` source with the upstream
panic-safety fix and regression test from commit
`f9a7f00fcf2d33e00adb03758cb350aaaa52cddb`. This addresses
RUSTSEC-2026-0253 while `zarrs 0.23.13` still requires `lru 0.16.x`. See
`vendor/lru/SECURITY-PATCH.md` for source, checksum, and removal criteria.

`vendor/wayland-scanner` is the crates.io `wayland-scanner 0.31.10` source with
only its `quick-xml` requirement changed from 0.39 to 0.41. This removes
RUSTSEC-2026-0194 and RUSTSEC-2026-0195 without importing unrelated unreleased
`wayland-rs` changes. See `vendor/wayland-scanner/SECURITY-PATCH.md` for source,
license, upstream commit, and removal criteria.

## Time-bounded advisory exceptions

- RUSTSEC-2021-0153: `encoding 0.2.33` is unmaintained and enters through
  `dicom-encoding 0.9.1`. No safe direct upgrade is available.
- RUSTSEC-2024-0436: `paste 1.0.15` is unmaintained and enters through
  `metal 0.33`. No safe direct upgrade is available.

These are maintenance warnings, not known vulnerabilities. Review both by
2026-10-01 or when the corresponding upstream dependency releases, whichever
comes first. New vulnerabilities and unsound advisories remain denied.
