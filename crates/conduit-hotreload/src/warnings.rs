//! The feature-off warning for `hotReload`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::HotReloadConfig;

/// `true` when this build has the `hotreload` feature. The root crate asserts at compile time that its own `hotreload`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "hotreload");

/// The warning for `hotReload` that this build ignores because `hotreload` is not compiled in; `None` when the
/// feature is compiled in or `hotReload` is absent.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, config: Option<&HotReloadConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].hotReload is configured but Conduit was compiled without the \
         `hotreload` feature — browser hot-reload (the SSE stream and file watcher) will \
         be disabled. \
         Recompile with `--features hotreload` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].hotReload is configured but Conduit was compiled without the \
             `hotreload` feature — browser hot-reload (the SSE stream and file watcher) \
             will be disabled. Recompile with `--features hotreload` to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(3, None), None);
    }
}
