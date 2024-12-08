#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the nightly toolchain
# used by Kani verifier.
# cargo update crate-name --precise x.y.z
