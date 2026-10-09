---
name: new-feature-crate
description: Recipe for adding a NEW feature to the 2.0 workspace end to end (crate + Cargo feature + config + feature warning + runtime hook + tests + schema/docs/CI), derived from the real touchpoints of #129–#148 (issue #259); plus the checklist for promoting a crate to standalone quality (#258). Load before implementing any new feature, or when running the `crate-steward` agent.
---

# New feature crate — what a feature really touches

Derived 2026-10-03 from how `conduit-faults` (the smallest representative one) is wired, cross-checked against #145–#148.
Moving **existing** code into a crate is a different job: `CONTRIBUTING.md` "Cargo Workspace Crate Extraction Recipe". This file is the single recipe for #259 (the earlier duplicate in `CONTRIBUTING.md` was merged into it).

## Before the first line of code
- Prior-art note in the issue and the RFC section for anything wire-visible (`rules/workflow.md` "Research before building").
- New dependency → `lawyer`. Decide gating with decision #31: heavy → real `--features` flag; light → always-on.
- Name: crate `conduit-<name>` (package `lopatnov-conduit-<name>`), feature `<name>`, config key `camelCase`.

## Touchpoints (each one was needed in practice; the faults paths are the model)
1. **`crates/conduit-<name>/`** — `Cargo.toml` inherits `[workspace.package]`; config types need only `serde` and are **always
   compiled** (so a config for a missing feature parses and can warn); everything else (`guard.rs`/`handler.rs`, heavy deps) is
   `optional = true` behind the crate's own feature `<name>`. `warnings.rs`: `pub const COMPILED: bool = cfg!(feature = "<name>")`
   and `feature_warning(i, cfg) -> Option<String>`, with a unit test that pins the text. Lib name is `conduit_<name>`, package
   `lopatnov-conduit-<name>` (decision #32).
2. **Root `Cargo.toml`** — `crates/conduit-<name>` in `[workspace].members`, `[workspace.dependencies]` entry (`path` + `version = "2.0.0"`), the mandatory dependency, the feature
   `<name> = ["lopatnov-conduit-<name>/<name>", "lopatnov-conduit-runtime/<name>", …every crate that gates code on it]`, membership in
   `standard`/`full`/profile lists (a niche or heavy feature — chaos, scripting/WASM — goes in `full` only), `[[test]]
   required-features`. Keep the version string in lockstep: `scripts/check-workspace-versions.sh`.
3. **`conduit-config`** — the field on `SiteConfig`/`GlobalConfig` (`schema/*.rs`), `#[serde(deny_unknown_fields)]` on the new struct
   (accepting unknown keys silently bit us, #510), re-export through the root facade `src/config/schema/mod.rs`.
4. **`conduit-runtime`** — feature forwarding + `features::<NAME>` const in `lib.rs`; the guard pushed in `filter/chain.rs`
   (response filters in `filter/response_chain.rs`: declare `may_block`, default `true`); per-request state as a `#[cfg]` field on
   `RequestCtx` (decision #30). `service.rs` stays untouched.
5. **`conduit-server`** — feature forwarding; the key in the `(camelKey, feature)` table and the `feature_warning(...)` call in
   `config/validate/warnings.rs` (its position in `check_site_simple_feature_warnings` is the warning's position in the output);
   the validator in the crate's `validate.rs`, **called from `config/validate/site.rs`** (an uncalled validator compiles and silently
   passes invalid configs); if the key can land in `SiteConfig.extra` when the feature is off, add it to
   `DISABLED_KEY_OWNING_FEATURE` so the "unknown key" warning becomes "recompile with --features x".
6. **Root parity assert** in `src/config/validate/mod.rs`: `COMPILED == cfg!(feature = "<name>")`. A missed forward is silent —
   negative control: delete one forward, the build must fail with E0080.
7. **Tests** — unit tests in the crate; golden files `src/config/validate/testdata/*.golden` (+ `golden_tests.rs`, `tests.rs`) for
   `validate()`/`feature_warnings()`; integration `tests/<name>.rs` with a raw `TcpListener` mock (`testing` skill); a negative
   control for every guard decision. Tests that evaluate the *root's* features stay in the root crate.
8. **Schema and docs** — `schema/conduit.schema.json` (hand-synced; CI `scripts/check_schema_superset.py` fails when a field is
   missing; validate with `node -e "JSON.parse(fs.readFileSync('schema/conduit.schema.json','utf8'))"`), `docs/configuration.md`, `docs/building.md` feature table, README feature table, `examples/<name>.{yaml,json}`
   (validated by tests), `CHANGELOG.md`, `crates/README.md`. Grep every changed feature name across all of them by content, not by an expected keyword.
9. **CI and tooling** — every `ci.yml` line that lists `-p lopatnov-conduit-runtime` also needs the new package (otherwise its tests
   silently stop running in that job); `scripts/verify-local.sh` feature lists; `sonar-project.properties` paths.

## Proof before the PR
`scripts/verify-local.sh` (not a hand-written chain), `cargo hack --workspace --each-feature --no-dev-deps`, the clippy matrix, the
Footprint report's size delta, one `security-engineer` pass on the final head, and a "Found while here" table in the PR.

## Gotchas that cost time
`git mv` (not delete+create) keeps rename detection; `cargo hack` rewrites manifests while it runs — never `git add` meanwhile;
never patch Rust source with a Python heredoc that contains backslashes (use `Edit`); `cfg!()` is crate-relative, hence the root
parity asserts; the orphan rule decides where `impl`s live; Windows full-feature runs can flake (#477) — rerun the binary alone.

## Promoting a crate to standalone quality (#258 pilot)
README with a runnable example; `[package.metadata.docs.rs]`; `#![warn(missing_docs)]` on the public surface and an explicit
list of what is public (everything else `pub(crate)`); `cargo-semver-checks` in CI; its own `CHANGELOG.md`; decide lockstep versus
independent versioning before publishing. Check with `cargo tree -i` that it does not depend on `conduit-config`/`conduit-runtime`.
