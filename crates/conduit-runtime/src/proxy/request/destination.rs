//! WebDAV `Destination` and `If` header rewriting (`proxy.*.rewriteDestination`, opt-in).
//!
//! A route that strips a prefix or rewrites the path changes the request line, but `COPY` and `MOVE` name their target
//! in `Destination` (RFC 4918 section 10.3: an absolute URI or an absolute path) and a tagged `If` header names the
//! resources its conditions apply to (section 10.4: `<URL>` outside the parentheses). Both still carry the public path,
//! so an upstream that serves the stripped path would not find them. With the option on, the same strip and rewrite
//! rules are applied to those paths. Only the path of a URL changes: the scheme, the authority, the query and the state
//! tokens and entity tags inside the parentheses of `If` stay as the client wrote them, and a URL naming another
//! authority than the request's `Host` is left alone (it is not a resource of this route).

use conduit_config::schema::RewriteRule;
use pingora_http::RequestHeader;

use super::transform::{apply_path_rewrites, apply_path_strip};

/// Apply the route's strip and rewrite rules to the `Destination` and `If` headers of `req`.
///
/// `host` is the request's `Host` (with a port, as the client sent it).
pub(super) fn rewrite_webdav_headers(
    req: &mut RequestHeader,
    host: &str,
    strip_prefix: Option<&str>,
    rewrite: Option<&[RewriteRule]>,
) {
    for name in ["destination", "if"] {
        let old: Vec<String> = req
            .headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_owned))
            .collect();
        if old.is_empty() {
            continue;
        }
        let map = |path: &str| map_path(path, strip_prefix, rewrite);
        let new: Vec<String> = old
            .iter()
            .map(|v| {
                if name == "destination" {
                    rewrite_url(v.trim(), host, &map)
                } else {
                    rewrite_if(v, host, &map)
                }
            })
            .collect();
        if new == old {
            continue;
        }
        // A value that is not a legal header value is dropped by `append_header`'s error; keep the original then.
        req.remove_header(name);
        for (value, original) in new.iter().zip(&old) {
            if req.append_header(name.to_owned(), value.as_str()).is_err() {
                let _ = req.append_header(name.to_owned(), original.as_str());
            }
        }
    }
}

/// The request-line transform of a route applied to one path. A path outside the route's prefix is not touched (the
/// request line itself can never carry one, `Destination` can).
fn map_path(path: &str, strip_prefix: Option<&str>, rewrite: Option<&[RewriteRule]>) -> String {
    let stripped = match strip_prefix {
        // The router's rule: the prefix must end at a segment boundary (`/mv` matches `/mv` and `/mv/..`, not `/mvx`).
        Some(prefix)
            if path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/')) =>
        {
            apply_path_strip(path, Some(prefix))
        }
        Some(_) => return path.to_owned(),
        None => path.to_owned(),
    };
    apply_path_rewrites(&stripped, rewrite)
}

/// Rewrite the path of an absolute URI or an absolute path; anything else (a relative reference, a URI of another
/// authority) is returned unchanged.
fn rewrite_url(url: &str, host: &str, map: &dyn Fn(&str) -> String) -> String {
    let (prefix, rest) = if let Some(after) = url.find("://").map(|i| i + 3) {
        let authority_end = url[after..]
            .find(['/', '?', '#'])
            .map_or(url.len(), |i| after + i);
        let authority = &url[after..authority_end];
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        if !authority.eq_ignore_ascii_case(host) {
            return url.to_owned();
        }
        (&url[..authority_end], &url[authority_end..])
    } else if url.starts_with('/') {
        ("", url)
    } else {
        return url.to_owned();
    };
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let (path, tail) = rest.split_at(path_end);
    if !path.starts_with('/') {
        return url.to_owned();
    }
    let mapped = map(path);
    // A rewrite rule (or a path like `/dav//evil.com/x`) can produce something that is not a plain absolute path: pasted
    // behind an authority it would change the authority (`http://h` + `x/a` is `http://hx/a`), and on its own `//evil.com/x`
    // is a network-path reference. Such a URL is left as the client wrote it.
    if !mapped.starts_with('/') || mapped.starts_with("//") {
        return url.to_owned();
    }
    format!("{prefix}{mapped}{tail}")
}

/// Rewrite the resource tags of an `If` header: `<URL>` outside the parentheses. What is inside the parentheses (state
/// tokens like `<urn:uuid:...>` and entity tags `[...]`) is copied through.
fn rewrite_if(value: &str, host: &str, map: &dyn Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0u32;
    let mut chars = value.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '(' => {
                depth += 1;
                out.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                out.push(c);
            }
            '[' if depth > 0 => {
                // An entity tag: copy through the closing bracket, honouring quotes ("a]b" is one tag).
                out.push(c);
                let mut quoted = false;
                for (_, d) in chars.by_ref() {
                    out.push(d);
                    match d {
                        '"' => quoted = !quoted,
                        ']' if !quoted => break,
                        _ => {}
                    }
                }
            }
            '<' => {
                let end = value[i + 1..].find('>').map(|j| i + 1 + j);
                match end {
                    Some(end) => {
                        let tag = &value[i + 1..end];
                        out.push('<');
                        if depth == 0 {
                            out.push_str(&rewrite_url(tag, host, map));
                        } else {
                            out.push_str(tag);
                        }
                        out.push('>');
                        // Skip what was copied.
                        while chars.peek().is_some_and(|(k, _)| *k <= end) {
                            chars.next();
                        }
                    }
                    None => {
                        // Unterminated tag: copy the rest as it is.
                        out.push_str(&value[i..]);
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_api(p: &str) -> String {
        map_path(p, Some("/api"), None)
    }

    #[test]
    fn destination_path_loses_the_stripped_prefix() {
        assert_eq!(
            rewrite_url("/api/files/b.txt", "h", &strip_api),
            "/files/b.txt"
        );
    }

    #[test]
    fn destination_uri_keeps_scheme_authority_and_query() {
        assert_eq!(
            rewrite_url("https://h:8443/api/a%20b?x=1", "h:8443", &strip_api),
            "https://h:8443/a%20b?x=1"
        );
    }

    #[test]
    fn destination_of_another_authority_is_left_alone() {
        assert_eq!(
            rewrite_url("https://other/api/a", "h", &strip_api),
            "https://other/api/a"
        );
    }

    #[test]
    fn destination_outside_the_prefix_is_left_alone() {
        assert_eq!(rewrite_url("/elsewhere/a", "h", &strip_api), "/elsewhere/a");
    }

    #[test]
    fn a_sibling_that_only_shares_the_prefix_text_is_left_alone() {
        // `/api` is not a prefix of `/apix/a.txt` at a segment boundary: the authority must not change.
        assert_eq!(
            rewrite_url("http://h/apix/a.txt", "h", &strip_api),
            "http://h/apix/a.txt"
        );
        assert_eq!(rewrite_url("/apix/a", "h", &strip_api), "/apix/a");
        assert_eq!(rewrite_url("/api", "h", &strip_api), "/");
        assert_eq!(rewrite_url("http://h/api", "h", &strip_api), "http://h/");
    }

    #[test]
    fn a_stripped_path_cannot_turn_into_another_authority() {
        let strip_dav = |p: &str| map_path(p, Some("/dav"), None);
        // `/dav.attacker.net/x` is not under `/dav`; `http://example.com` + `.attacker.net/x` must never be built.
        assert_eq!(
            rewrite_url(
                "http://example.com/dav.attacker.net/x",
                "example.com",
                &strip_dav
            ),
            "http://example.com/dav.attacker.net/x"
        );
        assert_eq!(
            rewrite_url("/dav.attacker.net/x", "example.com", &strip_dav),
            "/dav.attacker.net/x"
        );
        // `/dav//evil.com/x` strips to `//evil.com/x`, a network-path reference: left as written.
        assert_eq!(
            rewrite_url("/dav//evil.com/x", "example.com", &strip_dav),
            "/dav//evil.com/x"
        );
        assert_eq!(
            rewrite_url(
                "http://example.com/dav//evil.com/x",
                "example.com",
                &strip_dav
            ),
            "http://example.com/dav//evil.com/x"
        );
        // The same in an If resource tag.
        assert_eq!(
            rewrite_if(
                "</dav//evil.com/x> (<urn:uuid:1>) </dav.attacker.net/x>",
                "example.com",
                &strip_dav
            ),
            "</dav//evil.com/x> (<urn:uuid:1>) </dav.attacker.net/x>"
        );
    }

    #[test]
    fn a_rewrite_that_is_not_an_absolute_path_leaves_the_url_alone() {
        let rules = vec![RewriteRule {
            from: "^/rw/".to_owned(),
            to: "x/".to_owned(),
        }];
        let map = |p: &str| map_path(p, None, Some(&rules));
        assert_eq!(rewrite_url("http://h/rw/a", "h", &map), "http://h/rw/a");
        let rules = vec![RewriteRule {
            from: "^/rw/".to_owned(),
            to: "//evil.com/".to_owned(),
        }];
        let map = |p: &str| map_path(p, None, Some(&rules));
        assert_eq!(rewrite_url("/rw/a", "h", &map), "/rw/a");
    }

    #[test]
    fn destination_authority_with_userinfo_is_compared_without_it() {
        assert_eq!(
            rewrite_url("http://u:p@h/api/a", "h", &strip_api),
            "http://u:p@h/a"
        );
    }

    #[test]
    fn rewrite_rules_apply_to_the_destination() {
        let rules = vec![RewriteRule {
            from: "^/rw/".to_owned(),
            to: "/store/".to_owned(),
        }];
        assert_eq!(map_path("/rw/x", None, Some(&rules)), "/store/x");
    }

    #[test]
    fn if_resource_tags_are_rewritten_and_state_tokens_are_not() {
        let v = r#"</api/a> (<urn:uuid:/api/x> ["/api/e"]) </api/b> (Not <DAV:no-lock>)"#;
        assert_eq!(
            rewrite_if(v, "h", &strip_api),
            r#"</a> (<urn:uuid:/api/x> ["/api/e"]) </b> (Not <DAV:no-lock>)"#
        );
    }

    #[test]
    fn if_entity_tag_with_a_bracket_inside_quotes_stays_whole() {
        let v = r#"</api/a> (["a]b" </api/not-a-tag>])"#;
        assert_eq!(
            rewrite_if(v, "h", &strip_api),
            r#"</a> (["a]b" </api/not-a-tag>])"#
        );
    }

    #[test]
    fn if_without_tags_and_unterminated_tags_survive() {
        assert_eq!(
            rewrite_if("(<urn:uuid:1>)", "h", &strip_api),
            "(<urn:uuid:1>)"
        );
        assert_eq!(rewrite_if("</api/a", "h", &strip_api), "</api/a");
    }

    #[test]
    fn headers_are_rewritten_on_the_request() {
        let mut req = RequestHeader::build("MOVE", b"/api/a", None).unwrap();
        req.insert_header("destination", "http://h/api/b").unwrap();
        req.insert_header("if", "</api/a> (<urn:uuid:1>)").unwrap();
        rewrite_webdav_headers(&mut req, "h", Some("/api"), None);
        assert_eq!(req.headers["destination"], "http://h/b");
        assert_eq!(req.headers["if"], "</a> (<urn:uuid:1>)");
    }

    #[test]
    fn requests_without_the_headers_are_untouched() {
        let mut req = RequestHeader::build("GET", b"/api/a", None).unwrap();
        rewrite_webdav_headers(&mut req, "h", Some("/api"), None);
        assert!(req.headers.get("destination").is_none());
        assert!(req.headers.get("if").is_none());
    }
}
