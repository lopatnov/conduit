//! The feature-off warning for a Redis `rateLimit.store`, called by the root crate's `config::validate::feature_warnings()`.

/// `true` when this build has the `redis` feature. The root crate asserts at compile time that its own `redis`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "redis");

/// The warning for `rateLimit.store` (a predicate the root evaluates, because it needs the whole site) that this build ignores because `redis` is not compiled in; `None` when the
/// feature is compiled in or nothing is configured.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, uses_redis: bool) -> Option<String> {
    if COMPILED || !uses_redis {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].rateLimit.store (site, route, or consumer level) uses Redis but \
         Conduit was compiled without the `redis` feature — falling back to in-memory \
         rate limiting everywhere. Recompile with `--features redis` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].rateLimit.store (site, route, or consumer level) uses Redis but \
             Conduit was compiled without the `redis` feature — falling back to \
             in-memory rate limiting everywhere. Recompile with `--features redis` to \
             enable."
        );
    }

    #[test]
    fn warns_only_when_configured_and_not_compiled() {
        assert_eq!(feature_warning(3, false), None);
        assert_eq!(feature_warning(3, true).is_some(), !COMPILED);
    }
}
