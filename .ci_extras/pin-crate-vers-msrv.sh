#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the MSRV.
cargo update -p dashmap --precise 5.4.0
cargo update -p tempfile --precise 3.6.0
