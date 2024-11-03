#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the nightly toolchain.
# https://github.com/tkaitchuck/aHash/issues/200
cargo update -p ahash --precise 0.8.7
