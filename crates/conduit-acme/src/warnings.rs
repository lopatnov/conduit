//! The feature-off warning for `tls.acme`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::AcmeConfig;

/// `true` when this build has the `acme` feature. The root crate asserts at compile time that its own `acme`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "acme");

/// The warning for `tls.acme` that this build ignores because `acme` is not compiled in; `None` when the
/// feature is compiled in or `tls.acme` is absent.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, config: Option<&AcmeConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].tls.acme is configured but Conduit was compiled without the `acme` \
         feature — automatic TLS certificate provisioning will be disabled. \
         Recompile with `--features acme` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].tls.acme is configured but Conduit was compiled without the `acme` \
             feature — automatic TLS certificate provisioning will be disabled. \
             Recompile with `--features acme` to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(3, None), None);
    }
}
