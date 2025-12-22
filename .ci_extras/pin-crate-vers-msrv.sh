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
cargo update -p indexmap --precise 2.11.4
cargo update -p parking_lot --precise 0.12.4
cargo update -p parking_lot_core --precise 0.9.11
cargo update -p lock_api --precise 0.4.13
cargo update -p async-lock --precise 3.4.1
