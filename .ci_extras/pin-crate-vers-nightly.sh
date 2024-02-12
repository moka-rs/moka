#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the nightly toolchain.
cargo update -p openssl --precise 0.10.39
cargo update -p cc --precise 1.0.61
cargo update -p proc-macro2 --precise 1.0.63
cargo update -p parking_lot_core --precise 0.9.3
cargo update -p dashmap --precise 5.4.0
# https://github.com/tkaitchuck/aHash/issues/200
cargo update -p ahash --precise 0.8.7
