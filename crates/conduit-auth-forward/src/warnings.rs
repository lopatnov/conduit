//! The feature-off warning for `forwardAuth`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::ForwardAuthConfig;

/// `true` when this build has the `forward-auth` feature. The root crate asserts at compile time that its own `forward-auth`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "forward-auth");

/// The warning for `forwardAuth` that this build ignores because `forward-auth` is not compiled in; `None` when the
/// feature is compiled in or `forwardAuth` is absent.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, config: Option<&ForwardAuthConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].forwardAuth is configured but Conduit was compiled without the \
         `forward-auth` feature — ForwardAuth will be disabled. \
         Recompile with `--features forward-auth` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].forwardAuth is configured but Conduit was compiled without the \
             `forward-auth` feature — ForwardAuth will be disabled. Recompile with \
             `--features forward-auth` to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(3, None), None);
    }

    #[test]
    fn warns_only_when_configured_and_not_compiled() {
        assert_eq!(
            feature_warning(3, Some(&ForwardAuthConfig::default())).is_some(),
            !COMPILED
        );
    }
}
