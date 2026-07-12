#!/bin/sh

set -eux

# Kani's bundled Cargo is nightly-2024-08-07 (v1.82.0-nightly), which predates
# the stabilization of the 2024 edition (Rust 1.85) and therefore cannot parse
# the manifests of edition-2024 crates. cargo-kani runs `cargo metadata
# --filter-platform <host>`, so any edition-2024 crate that resolves onto the
# host platform makes it fail with "feature `edition2024` is required". Downgrade
# / pin the offenders to their newest edition-2021 releases. Kani only verifies
# the library, so these dev/build dependencies are never actually compiled here.

# `reqwest` v0.12's dependency tree pulls in many edition-2024 crates (`zeroize`
# v1.9, the `rand` v0.10 stack, `idna_adapter`, ...). v0.11 uses an older,
# edition-2021 tree.
cargo remove --dev reqwest
cargo add --dev reqwest@0.11.11 --no-default-features --features rustls-tls

# Pin some dependencies to specific versions for the nightly toolchain
# used by Kani verifier.
cargo update -p async-lock --precise 3.4.1
cargo update -p uuid --precise 1.20.0
# `trybuild` v1.0.106+ requires the edition-2024 `toml` v1.x stack (which in turn
# forces `indexmap` >= 2.13), and v1.0.118 additionally bumped its own crate to
# edition 2024. Pin to v1.0.105 — the newest release using the edition-2021
# `toml` v0.8 stack. This must run before the `indexmap` pin below, otherwise
# `toml` v1.x keeps `indexmap` locked to >= 2.13. (`trybuild` is
# `cfg(trybuild)`-gated and never built here.)
cargo update -p trybuild --precise 1.0.105
# `url` v2.5.4+ switched to `idna` v1 / `idna_adapter` (edition 2024). Pin to
# v2.5.2, which still uses the edition-2021 `idna` v0.5.
cargo update -p url --precise 2.5.2
# `indexmap` v2.13.0+ is edition 2024. Pin to the last edition-2021 release.
cargo update -p indexmap --precise 2.11.4
