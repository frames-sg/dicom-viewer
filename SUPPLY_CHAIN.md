# Supply-chain policy

`cargo audit --deny unsound` and `cargo deny check advisories bans licenses
sources` are release gates. `deny.toml` is the machine-readable policy.

## Temporary security patch

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
