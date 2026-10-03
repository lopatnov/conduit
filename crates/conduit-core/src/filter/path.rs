//! Path-pattern matching shared by every `skipPaths`-style config field
//! (JWT, ForwardAuth, Consumers, ...).

/// Returns `true` if `path` matches any entry in `skip_paths`.
///
/// Pattern rules:
/// - `/prefix/**` — matches `/prefix`, `/prefix/`, and any sub-path
/// - anything else — exact match only
pub fn is_path_skipped(skip_paths: Option<&[String]>, path: &str) -> bool {
    let Some(paths) = skip_paths else {
        return false;
    };
    paths.iter().any(|p| path_matches(p, path))
}

// Not `pub`: pre-migration this was `pub(crate)`, and root's own
// `src/proxy/cache.rs` has an unrelated private `path_matches` with
// materially different semantics (its glob prefix-matches even without a
// trailing `/**`) — widening this to a crate-published API surface would
// be an accidental semver commitment once member crates start publishing
// to crates.io (issue #114, Phase-2 facade-checkpoint audit).
fn path_matches(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        path == prefix || path.starts_with(&format!("{prefix}/"))
    } else {
        pattern == path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_only() {
        assert!(path_matches("/foo", "/foo"));
        assert!(!path_matches("/foo", "/foobar"));
        assert!(!path_matches("/foo", "/foo/bar"));
    }

    #[test]
    fn glob_prefix_match() {
        assert!(path_matches("/foo/**", "/foo"));
        assert!(path_matches("/foo/**", "/foo/"));
        assert!(path_matches("/foo/**", "/foo/bar"));
        assert!(path_matches("/foo/**", "/foo/bar/baz"));
        assert!(!path_matches("/foo/**", "/foobar"));
        assert!(!path_matches("/foo/**", "/other"));
    }

    #[test]
    fn skip_paths_none_never_skips() {
        assert!(!is_path_skipped(None, "/any/path"));
    }

    #[test]
    fn skip_paths_exact_and_glob() {
        let paths = vec!["/__health__".to_string(), "/public/**".to_string()];
        assert!(is_path_skipped(Some(&paths), "/__health__"));
        assert!(is_path_skipped(Some(&paths), "/public/img.png"));
        assert!(!is_path_skipped(Some(&paths), "/private"));
        assert!(!is_path_skipped(Some(&paths), "/__health__/sub"));
    }

    #[test]
    fn glob_matches_exactly_the_prefix() {
        // `/foo/**` should match `/foo` itself (without trailing slash).
        assert!(path_matches("/api/**", "/api"));
    }

    #[test]
    fn glob_matches_prefix_with_slash() {
        assert!(path_matches("/api/**", "/api/"));
    }

    #[test]
    fn exact_does_not_match_subpath() {
        assert!(!path_matches("/exact", "/exact/sub"));
    }

    #[test]
    fn empty_pattern_matches_only_empty_path() {
        assert!(path_matches("", ""));
        assert!(!path_matches("", "/anything"));
    }
}
