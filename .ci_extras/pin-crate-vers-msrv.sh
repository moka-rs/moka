#!/bin/sh

set -eux

# Pin some dependencies to specific versions for the MSRV.
cargo update -p tempfile --precise 3.6.0
cargo update -p tokio --precise 1.29.1
cargo update -p async-global-executor --precise 2.3.1
cargo update -p async-executor --precise 1.5.1
cargo update -p blocking --precise 1.4.1
cargo update -p reqwest --precise 0.11.18
cargo update -p regex --precise 1.9.6
cargo update -p memchr --precise 2.6.2
cargo update -p h2 --precise 0.3.20
cargo update -p actix-rt --precise 2.8.0
