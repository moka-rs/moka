#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the MSRV.
cargo update -p url --precise 2.5.2
cargo update -p actix-rt --precise 2.10.0
