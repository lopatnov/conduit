//! The feature-off warning for a route `cache` block, called by the root crate's `config::validate::feature_warnings()`.

/// `true` when this build has the `cache` feature. The root crate asserts at compile time that its own `cache`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "cache");

/// The warning for `cache` (a predicate the root evaluates, because it needs the whole site) that this build ignores because `cache` is not compiled in; `None` when the
/// feature is compiled in or nothing is configured.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, has_cache: bool) -> Option<String> {
    if COMPILED || !has_cache {
        return None;
    }
    Some(feature_warning_text(i))
}

/// `true` when this build has the `disk-cache` feature (the disk store). The root crate asserts at compile time that
/// its own `disk-cache` feature agrees.
pub const DISK_COMPILED: bool = cfg!(feature = "disk-cache");

/// The warning for a route whose `cache.store` is `disk:<path>` in a build without the disk store (issue #645):
/// the route is not cached then, the same as a `redis://` store without the `redis` feature. `None` when the store
/// is compiled in or no route asks for it.
pub fn disk_feature_warning(i: usize, uses_disk_store: bool) -> Option<String> {
    if DISK_COMPILED || !uses_disk_store {
        return None;
    }
    Some(disk_feature_warning_text(i))
}

fn disk_feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}] has proxy routes with a `disk:` cache store but Conduit was compiled \
         without the `disk-cache` feature — those routes will not be cached. \
         Recompile with `--features disk-cache` to enable."
    )
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}] has proxy routes with cache configured but Conduit was compiled \
         without the `cache` feature — response caching will be disabled. \
         Recompile with `--features cache` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3] has proxy routes with cache configured but Conduit was compiled \
             without the `cache` feature — response caching will be disabled. Recompile \
             with `--features cache` to enable."
        );
    }

    #[test]
    fn disk_feature_warning_text_is_pinned() {
        assert_eq!(
            disk_feature_warning_text(2),
            "sites[2] has proxy routes with a `disk:` cache store but Conduit was compiled \
             without the `disk-cache` feature — those routes will not be cached. Recompile \
             with `--features disk-cache` to enable."
        );
        assert_eq!(disk_feature_warning(2, false), None);
        assert_eq!(disk_feature_warning(2, true).is_some(), !DISK_COMPILED);
    }

    #[test]
    fn warns_only_when_configured_and_not_compiled() {
        assert_eq!(feature_warning(3, false), None);
        assert_eq!(feature_warning(3, true).is_some(), !COMPILED);
    }
}
