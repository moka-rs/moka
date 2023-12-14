#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the nightly toolchain.
cargo update -p proc-macro2 --precise 1.0.60
