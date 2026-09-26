//! The feature-off warning for `compression`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::CompressionConfig;

/// `true` when this build has the `compression` feature. The root crate asserts at compile time that its own `compression`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "compression");

/// The warning for `compression` that this build ignores because `compression` is not compiled in; `None` when the
/// feature is compiled in or `compression` is absent.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, config: Option<&CompressionConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].compression is configured but Conduit was compiled without the \
         `compression` feature — response compression will be disabled. \
         Recompile with `--features compression` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].compression is configured but Conduit was compiled without the \
             `compression` feature — response compression will be disabled. Recompile \
             with `--features compression` to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(3, None), None);
    }
}
