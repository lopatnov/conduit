//! The feature-off warning for `upload`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::UploadConfig;

/// `true` when this build has the `upload` feature. The root crate asserts at compile time that its own `upload`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "upload");

/// The warning for `upload` that this build ignores because `upload` is not compiled in; `None` when the
/// feature is compiled in or `upload` is absent.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, config: Option<&UploadConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].upload is configured but Conduit was compiled without the `upload` \
         feature — file upload will be disabled. \
         Recompile with `--features upload` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].upload is configured but Conduit was compiled without the `upload` \
             feature — file upload will be disabled. Recompile with `--features upload` \
             to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(3, None), None);
    }
}
