#!/bin/sh

set -eux

# Downgrade reqwest v0.12.x in Cargo.toml to v0.11.11.
cargo remove --dev reqwest
cargo add --dev reqwest@0.11.11 --no-default-features --features rustls-tls

# Pin some dependencies to specific versions for the MSRV.
cargo update -p url --precise 2.5.2
cargo update -p actix-rt --precise 2.10.0
cargo update -p tokio --precise 1.47.1
cargo update -p tokio-rustls --precise 0.24.1
cargo update -p tokio-util --precise 0.7.16
cargo update -p mio --precise 1.0.4
# `trybuild` v1.0.106+ requires `toml = "^0.9"`, and v1.0.118 additionally bumped
# its own crate to edition 2024. Both pull in edition-2024 manifests (the `toml`
# 1.x stack, `serde_spanned` 1.x, `toml_parser`, `toml_writer`, ...) that Cargo
# 1.71.1 cannot even parse ("this version of Cargo is older than the `2024`
# edition"). Pin `trybuild` to v1.0.105 — the newest release that still requires
# `toml = "^0.8"`, whose entire dependency subtree remains on edition 2021.
# `trybuild` is only built under `--cfg trybuild` (which this job does not set),
# so pinning it to an older release has no effect on the tests we actually run.
cargo update -p trybuild --precise 1.0.105
# `indexmap` v2.13.0+ requires Rust 1.82+. Pin to the last 1.71.1-compatible release.
cargo update -p indexmap --precise 2.11.4
cargo update -p parking_lot --precise 0.12.4
cargo update -p parking_lot_core --precise 0.9.11
cargo update -p lock_api --precise 0.4.13
cargo update -p async-lock --precise 3.4.1
cargo update -p uuid --precise 1.20.0
