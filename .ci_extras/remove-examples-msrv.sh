#!/usr/bin/env bash

# Disable examples from the MSRV build.

set -eux

# Replace the source file of example at $1 with the template file $2.
function disable_example_sync() {
    local example_name="$1"

    mv ./examples/${example_name}.rs ./examples/${example_name}.rs.bak
    cat << EOF > ./examples/${example_name}.rs
fn main() {}
EOF

    echo "Disabled $example_name."
}

# `OnceLock` was introduced in 1.70.0.
disable_example_sync reinsert_expired_entries_sync
