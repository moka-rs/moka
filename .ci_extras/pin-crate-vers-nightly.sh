#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the nightly toolchain.
cargo update -p openssl --precise 0.10.39
cargo update -p cc --precise 1.0.61
