# crates/

Feature crates for the Conduit 2.0 workspace migration (see [#114](https://github.com/lopatnov/conduit/issues/114))
land here — one crate per Cargo feature, extracted incrementally per the phase
plan in that issue.

Each crate is named `lopatnov-conduit-<name>` and inherits shared metadata
(`version`, `edition`, `license`, `repository`) from `[workspace.package]` in
the root `Cargo.toml` via `<field>.workspace = true`.

## Members

- **`conduit-core`** (Phase 2.1, [#126](https://github.com/lopatnov/conduit/issues/126))
  — Layer-0 vocabulary: traits, outcome enums, and narrow context types with
  zero config knowledge (`RequestFilter`/`ResponseFilter`, `LocalHandlerImpl`,
  `is_path_skipped`, `AcceptEncoding`, `content_type`, `LogWriter`). Compiled
  into every build regardless of feature selection — the root crate's `src/`
  re-exports these through thin facades; concrete guards/filters and
  config-aware chain assembly stay in the root crate.
