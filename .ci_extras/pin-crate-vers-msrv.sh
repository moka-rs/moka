#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the MSRV.
cargo update -p tempfile --precise 3.6.0
