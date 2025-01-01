#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the MSRV.
cargo update -p url --precise 2.5.2
