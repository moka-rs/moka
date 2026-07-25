# ADR-0002: MSRV and edition policy: Rust 1.71.1, 2021 edition

- Status: Accepted
- Date: 2026-07-25 (backfilled; the 1.71.1 MSRV and 2021 edition were
  already in place and this ADR records the existing, ongoing policy)
- Related: `Cargo.toml` (`rust-version`, `edition`), README.md ("Minimum
  Supported Rust Versions" section), CLAUDE.md, `.github/workflows/CI.yml`

## Context

Moka is a widely used, low-level dependency; many downstream crates and
applications build on it without control over their own toolchain version.
`Cargo.toml` pins `rust-version = "1.71.1"` (released August 3, 2023) and
`edition = "2021"`. README's "Minimum Supported Rust Versions" table states
the same MSRV, 1.71.1, for both the `future` and `sync` features — the two
mandatory, cache-implementing features of the crate.

README states a rolling MSRV policy of at least six months: the MSRV for
the mandatory, cache-implementing features (`future`, `sync`) is updated
conservatively, while the MSRV for other features may move forward more
often, up to the latest stable. In both cases, increasing the MSRV is
explicitly *not* considered a semver-breaking change.

Separately, the Rust 2024 edition requires rustc 1.85 or newer (stabilized
February 2025) — far ahead of the current MSRV of 1.71.1 — so adopting it is
not an option today.

## Decision

- MSRV is Rust 1.71.1 for both the `sync` and `future` features, tracked in
  `Cargo.toml`'s `rust-version` field and mirrored in README's MSRV table.
- MSRV bumps for the mandatory features (`sync`, `future`) are conservative,
  following a rolling window of at least 6 months; MSRV for other, optional
  features may move forward more readily, up to the latest stable. An MSRV
  bump is not treated as a semver-breaking change.
- The crate stays on the 2021 edition throughout the v0.12.x series, because
  the 2024 edition requires a toolchain (1.85+) far newer than the current
  MSRV.
- The migration to the 2024 edition (with the accompanying MSRV raise to
  1.85 or newer) is planned for v0.13.0. The rolling MSRV policy would
  permit such a bump within the v0.12.x series, but it is deliberately
  deferred to the next minor version to minimize the impact on downstream
  users.

## Alternatives Considered

- **Track recent stable Rust instead of a rolling-window MSRV.** Rejected:
  moka is a widely-used foundational dependency, and downstream users need
  toolchain stability more than they need the newest language features.
- **Move to the 2024 edition now.** Blocked by the MSRV: edition 2024
  requires rustc 1.85+, well ahead of 1.71.1.
- **Raise the MSRV to 1.85+ and adopt the 2024 edition within the v0.12.x
  series.** Permitted by the rolling MSRV policy, but rejected: bundling
  the edition switch and the large MSRV jump into v0.13.0 keeps the
  v0.12.x series stable for downstream users.

## Consequences

- Contributors and AI coding agents must not introduce language syntax or
  standard-library APIs stabilized after Rust 1.71.1, in either the `sync`
  or `future` code paths.
- CI enforces the MSRV: `.github/workflows/CI.yml` builds and tests against
  Rust 1.71.1 explicitly (labeled `# MSRV`), so violations are caught
  automatically.
- Edition-2024-only idioms must be avoided throughout the v0.12.x series;
  the edition migration happens in v0.13.0.
- MSRV increases only follow README's stated rolling policy; ad hoc bumps
  to unblock a single feature are not permitted.
