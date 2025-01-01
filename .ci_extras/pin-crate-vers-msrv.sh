#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the MSRV.
# cargo update -p <crate> --precise <version>
cargo update -p actix-rt --precise 2.9.0
cargo update -p cc --precise 1.0.105
cargo update -p tokio-util --precise 0.7.11
cargo update -p tokio --precise 1.38.1
cargo update -p url --precise 2.5.2
cargo update -p quanta --precise 0.12.2
