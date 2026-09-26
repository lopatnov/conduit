//! The feature-off warning for `global.otlp`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::OtlpConfig;

/// `true` when this build has the `otlp` feature. The root crate asserts at compile time that its own `otlp`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "otlp");

/// The warning for `global.otlp` that this build ignores because `otlp` is not compiled in; `None` when the
/// feature is compiled in or `global.otlp` is absent.
pub fn feature_warning(config: Option<&OtlpConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text())
}

fn feature_warning_text() -> String {
    "global.otlp is configured but Conduit was compiled without the `otlp` feature \
     — OpenTelemetry tracing will be disabled. \
     Recompile with `--features otlp` to enable."
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(),
            "global.otlp is configured but Conduit was compiled without the `otlp` \
             feature — OpenTelemetry tracing will be disabled. Recompile with \
             `--features otlp` to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(None), None);
    }
}
