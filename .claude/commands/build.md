---
description: Check fmt/clippy/tests via the build-validator subagent and return a compact GREEN/RED verdict — without dumping raw cargo output into the main context.
argument-hint: "[scope: empty = default features | 'full' = also check --features full]"
---

# /build — verification gate

Run conduit's standard checks: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`
(plus the `--features full` variants when relevant — see `CLAUDE.md` "Zero warnings" and
`.github/workflows/ci.yml` jobs `ci` / `ci-features`).

## What to do
1. Call the **`build-validator`** subagent (`.claude/agents/build-validator.md`).
2. If `$ARGUMENTS` contains `full` (or the change touches a feature-gated area — wasm, otlp,
   kubernetes, redis, acme, etc.) — ask it to also run the `--features full` variants.
   Otherwise default-feature checks are enough (faster, matches the common case).
3. Don't guess at commands — conduit's CI is the source of truth
   (`.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`).

## What to return
The structured report from `build-validator`:
`STATUS: GREEN|RED`, fmt/clippy/test numbers, top issues with file:line, and a recommendation
for what to fix next.

> A commit/PR is only acceptable when the build is green (0 errors, 0 warnings under
> `-D warnings`). Run `/build` after any non-trivial change, before committing, and before
> opening a PR.
