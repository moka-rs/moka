#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the MSRV.
# cargo update <crate> --precise <version>
cargo update actix-rt --precise 2.9.0
cargo update cc --precise 1.0.105
cargo update tokio-util --precise 0.7.11
cargo update tokio --precise 1.38.1
cargo update url --precise 2.5.2
