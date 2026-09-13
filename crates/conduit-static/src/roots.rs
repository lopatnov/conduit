//! Resolve a site's `static` config against a request path.
//!
//! Moved from `src/proxy/router.rs` (issue #114/#139) — see this crate's
//! `src/lib.rs` doc comment.

use std::path::PathBuf;

use crate::config::StaticConfig;

/// Resolve `cfg` into the filesystem root(s) to search and, for the `Mapped`
/// form, the URL path prefix to strip before joining onto the matched root.
///
/// Returns an empty `roots` vec when `cfg` is `Mapped` and no configured
/// prefix matches `path` — callers should treat that as "no static root
/// applies here" and fall through to their own fallback handling.
pub fn resolve_static_roots(cfg: &StaticConfig, path: &str) -> (Vec<PathBuf>, Option<String>) {
    match cfg {
        StaticConfig::Single(s) => (vec![PathBuf::from(s)], None),
        StaticConfig::Multi(v) => (v.iter().map(PathBuf::from).collect(), None),
        StaticConfig::Mapped(m) => match find_best_mapped_prefix(m, path) {
            Some((pfx, root)) => (vec![PathBuf::from(root)], Some(pfx.to_string())),
            None => (vec![], None),
        },
    }
}

/// Find the longest prefix in a mapped static config that matches `path`.
fn find_best_mapped_prefix<'a>(
    m: &'a indexmap::IndexMap<String, String>,
    path: &str,
) -> Option<(&'a str, &'a str)> {
    let mut best: Option<(&str, &str)> = None;
    for (prefix, root) in m {
        let norm = prefix.trim_end_matches('/');
        let matches = norm.is_empty() || path == norm || path.starts_with(&format!("{norm}/"));
        if matches {
            let len = norm.len();
            if best.is_none_or(|(b, _)| len > b.trim_end_matches('/').len()) {
                best = Some((prefix.as_str(), root.as_str()));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_roots_single() {
        let (roots, strip) = resolve_static_roots(&StaticConfig::Single("./dist".to_string()), "/");
        assert_eq!(roots, vec![PathBuf::from("./dist")]);
        assert!(strip.is_none());
    }

    #[test]
    fn static_roots_multi() {
        let (roots, strip) = resolve_static_roots(
            &StaticConfig::Multi(vec!["./a".to_string(), "./b".to_string()]),
            "/",
        );
        assert_eq!(roots, vec![PathBuf::from("./a"), PathBuf::from("./b")]);
        assert!(strip.is_none());
    }

    #[test]
    fn static_roots_mapped_matches_prefix() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("/docs".to_string(), "./docs-root".to_string());
        m.insert("/".to_string(), "./web".to_string());
        let (roots, strip) = resolve_static_roots(&StaticConfig::Mapped(m), "/docs/guide");
        assert_eq!(roots.len(), 1);
        assert!(roots[0].to_str().unwrap().contains("docs-root"));
        assert_eq!(strip.as_deref(), Some("/docs"));
    }

    #[test]
    fn static_roots_mapped_no_match_returns_empty() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("/docs".to_string(), "./docs-root".to_string());
        let (roots, _) = resolve_static_roots(&StaticConfig::Mapped(m), "/other");
        assert!(roots.is_empty());
    }
}
