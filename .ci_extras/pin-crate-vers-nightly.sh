#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the nightly toolchain.
cargo update -p proc-macro2 --precise 1.0.63
# https://github.com/tkaitchuck/aHash/issues/200
cargo update -p ahash --precise 0.8.7
