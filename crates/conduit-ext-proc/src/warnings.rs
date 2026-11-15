//! The feature-off message for `ext_proc` middleware entries.

/// `true` when this build has the `ext-proc` feature. The root crate asserts at compile time that its own `ext-proc`
/// feature agrees (`src/config/validate/mod.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "ext-proc");

/// The warning for an `ext_proc` entry that this build cannot run because `ext-proc` is not compiled in. A `failOpen: true`
/// entry is skipped (requests pass without being sent to the processor); a fail-closed one is also a validation error
/// ([`feature_error_text`]), so the same configuration never starts. Both warnings name the feature, which is the
/// contract of `feature_warnings()` that `conduit features` relies on.
pub fn feature_warning_text(i: usize, j: usize, fail_open: bool) -> String {
    if fail_open {
        format!(
            "sites[{i}].middleware[{j}] has type \"ext_proc\" (failOpen: true) but Conduit was compiled without the \
             `ext-proc` feature — requests will pass without being sent to the external processor. \
             Recompile with `--features ext-proc` to enable."
        )
    } else {
        format!(
            "sites[{i}].middleware[{j}] has type \"ext_proc\" but Conduit was compiled without the `ext-proc` \
             feature — this configuration is refused, because the external processor cannot be asked. \
             Recompile with `--features ext-proc` to enable."
        )
    }
}

/// The validation error for a fail-closed `ext_proc` entry in a build without the `ext-proc` feature.
pub fn feature_error_text() -> &'static str {
    "middleware type \"ext_proc\" needs a build with the `ext-proc` feature: without it the external processor would be \
     skipped and every request would pass unchecked. Recompile with `--features ext-proc`, or set failOpen: true to accept \
     that requests pass without it."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_pinned() {
        assert_eq!(
            feature_warning_text(3, 5, true),
            "sites[3].middleware[5] has type \"ext_proc\" (failOpen: true) but Conduit was compiled without the `ext-proc` \
             feature — requests will pass without being sent to the external processor. Recompile with `--features \
             ext-proc` to enable."
        );
        assert_eq!(
            feature_warning_text(3, 5, false),
            "sites[3].middleware[5] has type \"ext_proc\" but Conduit was compiled without the `ext-proc` feature — this \
             configuration is refused, because the external processor cannot be asked. Recompile with `--features \
             ext-proc` to enable."
        );
        assert!(feature_error_text().contains("--features ext-proc"));
    }
}
