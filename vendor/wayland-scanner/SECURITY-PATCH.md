# Temporary wayland-scanner security patch

This directory vendors the unmodified source of `wayland-scanner 0.31.10`
from crates.io, except for its `quick-xml` dependency requirement:

```toml
quick-xml = "0.41"
```

The patch addresses RUSTSEC-2026-0194 and RUSTSEC-2026-0195 while preserving
the released scanner implementation. Smithay made the same dependency upgrade
upstream in commit `d07c4f91f28b42e5a485823ffd9d8d5a210b1053`, but that commit is
based on other unreleased `wayland-rs` changes and cannot safely be pinned as a
drop-in replacement for the published crate.

Source: <https://crates.io/crates/wayland-scanner/0.31.10>
Upstream fix: <https://github.com/Smithay/wayland-rs/commit/d07c4f91f28b42e5a485823ffd9d8d5a210b1053>
License: MIT (`LICENSE.txt`)

Remove `vendor/wayland-scanner` and the root `[patch.crates-io]` entry after a
compatible crates.io release requires `quick-xml >= 0.41`.
