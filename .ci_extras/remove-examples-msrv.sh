#!/bin/sh

# Remove examples from the MSRV build.

set -eux

# Remove this because `OnceLock` was introduced in 1.70.0.
rm ./examples/reinsert_expired_entries_sync.rs
