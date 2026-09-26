//! The feature-off warnings for `static` and `fallback`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::{FallbackConfig, StaticConfig};

/// `true` when this build has the `static` feature. The root crate asserts at compile time that its own `static`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "static");

/// The warning for a `static` block that this build ignores because `static` is not compiled in; `None` when the
/// feature is compiled in or `static` is absent. `i` is the site index used in the message.
pub fn static_feature_warning(i: usize, config: Option<&StaticConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(static_feature_warning_text(i))
}

fn static_feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].static is configured but Conduit was compiled without the `static` \
         feature — static file serving will be disabled. \
         Recompile with `--features static` to enable."
    )
}

/// The warning for a `fallback` block that this build ignores because `static` (the feature that also serves fallback
/// responses) is not compiled in; `None` when the feature is compiled in or `fallback` is absent.
pub fn fallback_feature_warning(i: usize, config: Option<&FallbackConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(fallback_feature_warning_text(i))
}

fn fallback_feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].fallback is configured but Conduit was compiled without the `static` \
         feature — fallback responses (including the site's default 404) will be \
         disabled. \
         Recompile with `--features static` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_pinned() {
        assert_eq!(
            static_feature_warning_text(3),
            "sites[3].static is configured but Conduit was compiled without the `static` \
             feature — static file serving will be disabled. Recompile with `--features \
             static` to enable."
        );
        assert_eq!(
            fallback_feature_warning_text(3),
            "sites[3].fallback is configured but Conduit was compiled without the \
             `static` feature — fallback responses (including the site's default 404) \
             will be disabled. Recompile with `--features static` to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(static_feature_warning(3, None), None);
        assert_eq!(fallback_feature_warning(3, None), None);
    }

    #[test]
    fn fallback_warns_only_when_configured_and_not_compiled() {
        assert_eq!(
            fallback_feature_warning(3, Some(&FallbackConfig::default())).is_some(),
            !COMPILED
        );
    }
}
