#!/bin/sh

set -eu

rustup toolchain install stable -c clippy,rust-analysis,rust-src,rustfmt
rustup toolchain install beta   -c clippy
rustup toolchain install 1.70.0
rustup default stable
