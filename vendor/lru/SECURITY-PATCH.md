# Temporary lru security patch

This directory vendors `lru 0.16.4` from crates.io and backports only the
upstream fix and regression test for RUSTSEC-2026-0253. The fix detaches a
removed node before freeing it and running the key's potentially panicking
`Drop`, preventing the intrusive list from retaining dangling pointers.

- Crates.io source: <https://crates.io/crates/lru/0.16.4>
- Source archive SHA-256:
  `7f66e8d5d03f609abc3a39e6f08e4164ebf1447a732906d39eb9b99b7919ef39`
- Source revision recorded by crates.io:
  `d8c7f5ca51a86a8f561c14e21508a0f757aa05ad`
- Upstream fix and regression test:
  <https://github.com/jeromefroe/lru-rs/commit/f9a7f00fcf2d33e00adb03758cb350aaaa52cddb>
- Advisory: <https://rustsec.org/advisories/RUSTSEC-2026-0253.html>
- License: MIT (`LICENSE`)

The `cargo-audit` exception applies only because it identifies the crate by its
published `0.16.4` version and cannot detect a source-level backport. Cargo Deny
recognizes the local source as patched and needs no exception. The regression
test is part of the vendored crate's normal test suite.

Remove `vendor/lru`, the Cargo Audit exception, and the root patch entry when a
compatible `zarrs`/`zarrs_filesystem` release permits `lru >= 0.18.2`.
