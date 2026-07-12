#!/bin/sh

set -eux

# Downgrade reqwest v0.12.x in Cargo.toml to v0.11.11.
cargo remove --dev reqwest
cargo add --dev reqwest@0.11.11 --no-default-features --features rustls-tls

# Pin some dependencies to specific versions for the nightly toolchain.
# cargo update -p <crate> --precise <version>
# https://github.com/tkaitchuck/aHash/issues/200
cargo update -p ahash --precise 0.8.7

# `-Z minimal-versions` selects `rustix` v0.38.0, whose build script enables the
# `rustc_attrs` cfg and then uses the reserved `rustc_layout_scalar_valid_range_*`
# attributes, which the current nightly rejects ("attributes starting with
# `rustc` are reserved for use by the `rustc` compiler"). v0.38.44 only enables
# those attributes behind the opt-in `rustix_use_experimental_features` cfg, so
# it builds on nightly.
cargo update -p rustix --precise 0.38.44
