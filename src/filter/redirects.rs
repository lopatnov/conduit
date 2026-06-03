use crate::config::schema::RedirectRule;

/// Check whether any redirect rule matches `path` (including query string).
///
/// Returns `(location, status_code)` for the first matching rule, or `None`.
/// The query string from the original path is appended to the redirect location.
pub fn apply_redirects(rules: &[RedirectRule], path: &str) -> Option<(String, u16)> {
    // Separate path from query string — match on path only, preserve query in result.
    let (path_only, query) = path.split_once('?').unwrap_or((path, ""));

    for rule in rules {
        if let Some(mut location) = match_rule(&rule.from, &rule.to, path_only) {
            if !query.is_empty() {
                // If the target already contains '?' (e.g. a hard-coded query
                // string in rule.to), append with '&' to avoid "?a=1?b=2".
                location.push(if location.contains('?') { '&' } else { '?' });
                location.push_str(query);
            }
            let status = rule.status.unwrap_or(302);
            return Some((location, status));
        }
    }
    None
}

/// Match a single redirect rule against `path`.
///
/// Two modes:
/// - **Exact** (no `:param` in `from`): trailing slashes are normalised, then compared.
/// - **Param capture** (`:param` segments in `from`): path is split into segments
///   and matched 1-to-1; captured values are substituted into `to`.
fn match_rule(from: &str, to: &str, path: &str) -> Option<String> {
    if !from.contains(':') {
        // Exact match — normalise trailing slashes on both sides.
        let norm_from = from.trim_end_matches('/');
        let norm_path = path.trim_end_matches('/');
        if norm_path == norm_from {
            return Some(to.to_string());
        }
        return None;
    }

    // Param capture: split both patterns into non-empty segments and match 1-to-1.
    let from_segs: Vec<&str> = from.split('/').filter(|s| !s.is_empty()).collect();
    let path_segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if from_segs.len() != path_segs.len() {
        return None;
    }

    let mut captures: Vec<(&str, &str)> = Vec::new();
    for (f, p) in from_segs.iter().zip(path_segs.iter()) {
        if let Some(param) = f.strip_prefix(':') {
            captures.push((param, p));
        } else if *f != *p {
            return None;
        }
    }

    // Substitute captured values into the `to` template.
    // Sort by descending name length so that `:id2` is replaced before `:id`,
    // preventing shorter names from partially matching longer-named placeholders.
    captures.sort_by_key(|c| std::cmp::Reverse(c.0.len()));
    let mut result = to.to_string();
    for (name, value) in &captures {
        result = result.replace(&format!(":{name}"), value);
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::RedirectRule;

    fn rule(from: &str, to: &str, status: u16) -> RedirectRule {
        RedirectRule {
            from: from.into(),
            to: to.into(),
            status: Some(status),
        }
    }

    #[test]
    fn exact_301() {
        let rules = vec![rule("/old", "/new", 301)];
        assert_eq!(apply_redirects(&rules, "/old"), Some(("/new".into(), 301)));
    }

    #[test]
    fn no_match() {
        let rules = vec![rule("/old", "/new", 301)];
        assert!(apply_redirects(&rules, "/other").is_none());
    }

    #[test]
    fn trailing_slash_normalised() {
        let rules = vec![rule("/old/", "/new", 301)];
        assert_eq!(apply_redirects(&rules, "/old"), Some(("/new".into(), 301)));
        assert_eq!(apply_redirects(&rules, "/old/"), Some(("/new".into(), 301)));
    }

    #[test]
    fn param_capture_308() {
        let rules = vec![rule("/blog/:slug", "/posts/:slug", 308)];
        assert_eq!(
            apply_redirects(&rules, "/blog/hello-world"),
            Some(("/posts/hello-world".into(), 308))
        );
    }

    #[test]
    fn param_wrong_segment_count() {
        let rules = vec![rule("/blog/:slug", "/posts/:slug", 308)];
        // One extra segment — should not match.
        assert!(apply_redirects(&rules, "/blog/2024/post").is_none());
    }

    #[test]
    fn static_segment_mismatch() {
        let rules = vec![rule("/blog/:slug", "/posts/:slug", 308)];
        assert!(apply_redirects(&rules, "/news/hello-world").is_none());
    }

    #[test]
    fn query_string_preserved() {
        let rules = vec![rule("/old", "/new", 302)];
        assert_eq!(
            apply_redirects(&rules, "/old?foo=bar&baz=1"),
            Some(("/new?foo=bar&baz=1".into(), 302))
        );
    }

    #[test]
    fn query_string_preserved_with_param() {
        let rules = vec![rule("/blog/:slug", "/posts/:slug", 301)];
        assert_eq!(
            apply_redirects(&rules, "/blog/hello?ref=twitter"),
            Some(("/posts/hello?ref=twitter".into(), 301))
        );
    }

    #[test]
    fn default_status_302() {
        let rules = vec![RedirectRule {
            from: "/a".into(),
            to: "/b".into(),
            status: None,
        }];
        assert_eq!(apply_redirects(&rules, "/a"), Some(("/b".into(), 302)));
    }

    #[test]
    fn first_rule_wins() {
        let rules = vec![rule("/a", "/first", 301), rule("/a", "/second", 302)];
        assert_eq!(apply_redirects(&rules, "/a"), Some(("/first".into(), 301)));
    }

    #[test]
    fn external_redirect() {
        let rules = vec![rule("/docs", "https://docs.example.com", 302)];
        assert_eq!(
            apply_redirects(&rules, "/docs"),
            Some(("https://docs.example.com".into(), 302))
        );
    }

    #[test]
    fn multiple_params() {
        let rules = vec![rule("/users/:id/posts/:pid", "/u/:id/p/:pid", 301)];
        assert_eq!(
            apply_redirects(&rules, "/users/42/posts/7"),
            Some(("/u/42/p/7".into(), 301))
        );
    }

    #[test]
    fn query_appended_with_ampersand_when_target_has_query() {
        // When the target URL already has a '?', the incoming query is appended
        // with '&' to avoid "?a=1?b=2" double-question-mark.
        let rules = vec![rule("/old", "/new?utm=source", 302)];
        assert_eq!(
            apply_redirects(&rules, "/old?page=2"),
            Some(("/new?utm=source&page=2".into(), 302))
        );
    }

    #[test]
    fn query_appended_with_question_mark_when_target_has_no_query() {
        let rules = vec![rule("/old", "/new", 302)];
        assert_eq!(
            apply_redirects(&rules, "/old?page=2"),
            Some(("/new?page=2".into(), 302))
        );
    }
}
