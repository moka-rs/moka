# Architecture Decision Records

This directory records the "why" behind Moka's non-obvious design decisions,
capturing the context and reasoning at decision time so future maintainers —
human or AI — do not have to reverse-engineer it.

## Index

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-cht-clean-room-fork.md) | Maintain `src/cht` as a clean-room fork of the `cht` crate | Accepted |
| [0002](0002-msrv-and-edition-policy.md) | MSRV and edition policy: Rust 1.71.1, 2021 edition | Accepted |
| [0003](0003-caffeine-ports-apache2-only.md) | Keep the Caffeine-ported files Apache-2.0 only | Accepted |

## When to write an ADR

Write one when a change involves: the shape or semantics of the public API; a
concurrency protocol or memory-ordering choice; adopting or replacing a
dependency; MSRV, edition, or licensing policy; anything a maintainer would
ask "why was it done this way?" about a year later. Do NOT write one for
individual bug fixes (the issue/PR is enough), behavior-preserving
refactorings, or obvious choices. A short ADR (~20 lines) is fine — keep the
bar to writing one low.

## Rules

1. ADRs are immutable once accepted. To change a decision, write a new ADR
   that supersedes the old one, and update the old ADR's Status line to
   `Superseded by ADR-NNNN`. Nothing else in an accepted ADR may be edited.
2. Never renumber ADRs. Numbers are sequential, 4-digit, zero-padded. File
   names are `NNNN-short-kebab-title.md`.
3. Add new ADRs to the Index table above.
4. When an OpenSpec change (see `openspec/` at repo root) is archived, first
   distill the decisions from its `design.md` into a new ADR here; the change
   directory is disposable, the ADR is the permanent record.

## Template

````markdown
# ADR-NNNN: Title

- Status: Proposed | Accepted | Superseded by ADR-NNNN
- Date: YYYY-MM-DD
- Related: (issues, PRs, source files, other ADRs)

## Context

## Decision

## Alternatives Considered

## Consequences
````
