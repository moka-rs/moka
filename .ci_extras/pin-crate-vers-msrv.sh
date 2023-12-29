#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the MSRV.
cargo update -p cargo-platform --precise 0.1.5
