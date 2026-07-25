# ADR-0001: Maintain `src/cht` as a clean-room fork of the `cht` crate

- Status: Accepted
- Date: 2026-07-25 (backfilled; the decision dates from vendoring the hash
  table source into `src/cht` on 2022-02-01)
- Related: `src/cht/`, README.md ("Credits > cht" and "License" sections),
  possible future ADR on a `papaya`-based hash table (not yet written)

## Context

Moka's central key-value storage is a lock-free, resizable concurrent hash
table. Early versions of moka depended on the external `cht` crate (by
Gregory Meyer). Per README's "Credits > cht" section, the source files under
`moka::cht` were copied from `cht` v0.4.1 and modified by moka; "cht v0.4.1
and earlier are licensed under the MIT license." Moka needed invasive,
moka-specific changes — for example memory-layout changes to reduce
per-entry overhead, and the `RehashOp` abstraction in
`src/cht/map/bucket.rs`, which selects between full-table and segment-local
rehashing during a resize — that were impractical to maintain as an
out-of-tree patch set against an external dependency. The source was
imported into `src/cht` on 2022-02-01 via the maintainer's fork crate
`moka-cht`, and `RehashOp` was added that same day as part of the
memory-overhead reduction work; it has no counterpart in upstream `cht`
v0.4.1.

Upstream `cht` has since changed its license from MIT to a copyleft
(AGPL-family) license. Moka's own license is
`MIT OR Apache-2.0` (with two Caffeine-derived files under Apache-2.0 only,
see ADR-0003), so moka's fork can only ever track the last MIT-licensed
release, `cht` v0.4.1; it cannot incorporate any code, patch, or fix that
originates from a later, differently-licensed upstream without putting that
licensing at risk.

## Decision

`src/cht` is maintained as a clean-room fork of `cht` v0.4.1:

- Changes to `src/cht` must be developed independently, working from moka's
  own copy of the source and its own understanding of the algorithm.
- Contributors and AI coding agents must not copy code from, or write
  changes while consulting, any version of the upstream `cht` repository
  newer than v0.4.1 (i.e. any post-relicense revision).
- Upstream bug reports or issues against `cht` may be used only as a signal
  that "a similar bug may exist here"; any actual fix must be re-derived
  independently from moka's own source, never ported from upstream.

## Alternatives Considered

- **Keep depending on the external `cht` crate.** Rejected: the
  modifications moka needed were too invasive to maintain indefinitely as
  patches against an upstream dependency.
- **Switch to a different concurrent hash table implementation.** Not
  pursued at the time of vendoring. A possible future migration to
  `papaya` is under separate evaluation and, if it happens, will get its
  own ADR rather than being folded into this one.

## Consequences

- Moka carries the full maintenance burden for `src/cht`: bug fixes,
  performance work, and portability issues that an external dependency
  might otherwise have absorbed are moka's own responsibility.
- Contributors (human or AI) working on `src/cht` must treat `cht` v0.4.1 as
  the sole point of reference and avoid consulting later upstream source,
  to keep moka's `MIT OR Apache-2.0` licensing sound.
- `src/cht` has already diverged meaningfully from upstream (`RehashOp` is a
  moka-side addition with no upstream counterpart), so "porting a fix from
  upstream" is not a safe shortcut even before licensing is considered.
