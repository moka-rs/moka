#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the nightly toolchain
# used by Kani verifier.
cargo update -p async-lock --precise 3.4.1
cargo update -p uuid --precise 1.20.0
