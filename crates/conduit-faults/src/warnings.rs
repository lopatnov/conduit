//! The feature-off warning for `faultInjection`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::FaultInjectionConfig;

/// `true` when this build has the `fault-injection` feature. The root crate asserts at compile time that its own `fault-injection`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "fault-injection");

/// The warning for `faultInjection` that this build ignores because `fault-injection` is not compiled in; `None` when the
/// feature is compiled in or `faultInjection` is absent.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, config: Option<&FaultInjectionConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].faultInjection is configured but Conduit was compiled without the \
         `fault-injection` feature — fault injection will be disabled. \
         Recompile with `--features fault-injection` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].faultInjection is configured but Conduit was compiled without the \
             `fault-injection` feature — fault injection will be disabled. Recompile \
             with `--features fault-injection` to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(3, None), None);
    }

    #[test]
    fn warns_only_when_configured_and_not_compiled() {
        assert_eq!(
            feature_warning(3, Some(&FaultInjectionConfig::default())).is_some(),
            !COMPILED
        );
    }
}
