#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the nightly toolchain
# used by Kani verifier.
cargo update -p syn@2.0 --precise 2.0.58
cargo update -p proc-macro2 --precise 1.0.79
