//! The feature-off warning for `tcp`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::TcpConfig;

/// `true` when this build has the `tcp` feature. The root crate asserts at compile time that its own `tcp`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "tcp");

/// The warning for `tcp` that this build ignores because `tcp` is not compiled in; `None` when the
/// feature is compiled in or `tcp` is absent.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, config: Option<&TcpConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].tcp is configured but Conduit was compiled without the `tcp` \
         feature — TCP proxy mode will be disabled. \
         Recompile with `--features tcp` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].tcp is configured but Conduit was compiled without the `tcp` \
             feature — TCP proxy mode will be disabled. Recompile with `--features tcp` \
             to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(3, None), None);
    }

    #[test]
    fn warns_only_when_configured_and_not_compiled() {
        assert_eq!(
            feature_warning(3, Some(&TcpConfig::default())).is_some(),
            !COMPILED
        );
    }
}
