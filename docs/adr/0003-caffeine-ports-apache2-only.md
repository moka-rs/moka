# ADR-0003: Keep the Caffeine-ported files Apache-2.0 only

- Status: Accepted
- Date: 2026-07-25 (backfilled; `frequency_sketch.rs` was first added in
  2020 and `timer_wheel.rs` in 2023, both under Apache-2.0 from the start)
- Related: `src/common/frequency_sketch.rs`, `src/common/timer_wheel.rs`,
  `NOTICE`, README.md ("License" section, "Note on Licensing"), CLAUDE.md
  ("Important Licensing Notes")

## Context

`src/common/frequency_sketch.rs` (TinyLFU admission policy) and
`src/common/timer_wheel.rs` (hierarchical timer wheel for expiration) are
ports of Java classes from Ben Manes' Caffeine cache library
(`com.github.benmanes.caffeine.cache.FrequencySketch` and `...TimerWheel`
respectively). Both files carry a header stating that the ported code and
doc comments are licensed under the Apache License, Version 2.0, with
copyrights retained by Caffeine's contributors. The rest of the moka crate
is dual-licensed `MIT OR Apache-2.0` (`Cargo.toml`'s `license` field is
`"(MIT OR Apache-2.0) AND Apache-2.0"`, reflecting this split).

TinyLFU admission (via the frequency sketch) and per-entry expiration (via
the timer wheel) are central to moka's hit-ratio and expiration
characteristics; Caffeine's implementations of both are mature and
battle-tested in production use.

## Decision

`src/common/frequency_sketch.rs` and `src/common/timer_wheel.rs` remain
licensed solely under the Apache License 2.0, not dual-licensed. As
derivative works of Apache-2.0-licensed Caffeine code, they cannot be
offered under MIT terms. This exception is documented in `NOTICE`, in
README's "License" section, and in CLAUDE.md's "Important Licensing Notes".

## Alternatives Considered

- **Re-implement from the underlying papers** (the W-TinyLFU paper for the
  frequency sketch, general hierarchical timer wheel literature for
  expiration) **to allow uniform dual licensing.** Rejected: Caffeine's
  implementation quality and production track record outweighed the benefit
  of licensing uniformity across the crate.
- **Not adopt TinyLFU-based admission at all.** Rejected: TinyLFU admission
  is central to moka's hit-ratio characteristics and is a core part of its
  design goals.

## Consequences

- These two files are Apache-2.0 only; anyone redistributing them (alone or
  as part of moka) must comply with Apache-2.0 terms specifically for this
  code, rather than relying on the MIT option available for the rest of the
  crate. This is called out explicitly in `NOTICE` and README so
  downstream users doing license review are not surprised by it.
- Edits to these two files must preserve the existing license header and
  copyright/attribution notice at the top of the file; the header must not
  be removed or altered to imply dual licensing.
- Any new code ported or copied from Caffeine (or another Apache-2.0-only
  source) into other parts of moka would need the same treatment: an
  Apache-2.0-only header on the file and an update to `NOTICE`.
