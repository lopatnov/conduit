use base64::Engine as _;
use pingora_proxy::Session;

use crate::config::schema::{ApiKeyConfig, BasicAuthConfig, Consumer, ConsumersConfig};

/// Result of a Basic Auth credential check.
pub enum BasicAuthResult {
    Allowed,
    Denied { challenge: bool, realm: String },
}

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

pub(crate) fn path_matches(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        path == prefix || path.starts_with(&format!("{prefix}/"))
    } else {
        pattern == path
    }
}

/// Validate a raw `Authorization` header value against a user map.
///
/// Extracted for unit testability — `check_basic_auth` calls this after reading
/// the header value from the Pingora session.
///
/// RFC 7235: the scheme token is case-insensitive.
pub(crate) fn check_credentials(
    users: &indexmap::IndexMap<String, String>,
    auth_header: &str,
    challenge: bool,
    realm: String,
) -> BasicAuthResult {
    let (scheme, rest) = match auth_header.split_once(' ') {
        Some(pair) => pair,
        None => return BasicAuthResult::Denied { challenge, realm },
    };
    if !scheme.eq_ignore_ascii_case("Basic") {
        return BasicAuthResult::Denied { challenge, realm };
    }
    let b64 = rest.trim();

    let decoded = match base64::engine::general_purpose::STANDARD.decode(b64) {
        Ok(b) => b,
        Err(_) => return BasicAuthResult::Denied { challenge, realm },
    };

    let user_pass = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return BasicAuthResult::Denied { challenge, realm },
    };

    let (username, password) = match user_pass.split_once(':') {
        Some((u, p)) => (u, p),
        None => return BasicAuthResult::Denied { challenge, realm },
    };

    match users.get(username) {
        Some(expected) if expected == password => BasicAuthResult::Allowed,
        _ => BasicAuthResult::Denied { challenge, realm },
    }
}

/// Validate the `Authorization: Basic …` header against the configured user map.
pub fn check_basic_auth(cfg: &BasicAuthConfig, session: &Session) -> BasicAuthResult {
    let path = session.req_header().uri.path();
    if is_path_skipped(cfg.skip_paths.as_deref(), path) {
        return BasicAuthResult::Allowed;
    }

    let realm = cfg.realm.as_deref().unwrap_or("Restricted").to_owned();
    let challenge = cfg.challenge.unwrap_or(true);

    let auth_value = session
        .req_header()
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    check_credentials(&cfg.users, auth_value, challenge, realm)
}

/// Check whether the request carries a valid API key.
///
/// The key is read from the header named by `cfg.header` (default: `x-api-key`).
/// Returns `true` if the request is allowed.
pub fn check_api_key(cfg: &ApiKeyConfig, session: &Session) -> bool {
    let path = session.req_header().uri.path();
    if is_path_skipped(cfg.skip_paths.as_deref(), path) {
        return true;
    }

    let header_name = cfg.header.as_deref().unwrap_or("x-api-key");
    let provided = session
        .req_header()
        .headers
        .get(header_name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    cfg.keys.iter().any(|k| k == provided)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn users(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(u, p)| (u.to_string(), p.to_string()))
            .collect()
    }

    fn denied(r: BasicAuthResult) -> bool {
        matches!(r, BasicAuthResult::Denied { .. })
    }

    // ── check_credentials ─────────────────────────────────────────────────────

    #[test]
    fn credentials_missing_header_denied() {
        let u = users(&[("admin", "secret")]);
        assert!(denied(check_credentials(&u, "", true, "R".to_owned())));
    }

    #[test]
    fn credentials_wrong_scheme_denied() {
        let u = users(&[("admin", "secret")]);
        assert!(denied(check_credentials(
            &u,
            "Bearer token",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_invalid_base64_denied() {
        let u = users(&[("admin", "secret")]);
        assert!(denied(check_credentials(
            &u,
            "Basic not!valid!b64",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_no_colon_in_decoded_denied() {
        // base64("nocolon") = "bm9jb2xvbg=="
        let u = users(&[("admin", "secret")]);
        assert!(denied(check_credentials(
            &u,
            "Basic bm9jb2xvbg==",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_wrong_password_denied() {
        let u = users(&[("admin", "secret")]);
        // base64("admin:wrong") = "YWRtaW46d3Jvbmc="
        assert!(denied(check_credentials(
            &u,
            "Basic YWRtaW46d3Jvbmc=",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_correct_password_allowed() {
        let u = users(&[("admin", "secret")]);
        // base64("admin:secret") = "YWRtaW46c2VjcmV0"
        assert!(matches!(
            check_credentials(&u, "Basic YWRtaW46c2VjcmV0", true, "R".to_owned()),
            BasicAuthResult::Allowed
        ));
    }

    #[test]
    fn credentials_scheme_case_insensitive() {
        let u = users(&[("u", "p")]);
        // base64("u:p") = "dTpw"
        assert!(matches!(
            check_credentials(&u, "BASIC dTpw", false, "R".to_owned()),
            BasicAuthResult::Allowed
        ));
    }

    #[test]
    fn credentials_unknown_user_denied() {
        let u = users(&[("admin", "secret")]);
        // base64("other:secret") = "b3RoZXI6c2VjcmV0"
        assert!(denied(check_credentials(
            &u,
            "Basic b3RoZXI6c2VjcmV0",
            true,
            "R".to_owned()
        )));
    }

    // ── path_matches / is_path_skipped ────────────────────────────────────────

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
}

// ── Consumer model ─────────────────────────────────────────────────────────

/// Attempt to identify the request's consumer from the configured list.
///
/// Evaluation order: consumers are checked in declaration order; the **first
/// matching** consumer wins.  A consumer can use one of two credential types:
///
/// - **API key** — value in the `apiKeyHeader` request header (default:
///   `x-api-key`).  Constant-time comparison (`==`) is used.
/// - **Basic Auth** — `Authorization: Basic <base64(username:password)>` where
///   the username must equal `consumer.username`.
///
/// Returns `None` when no consumer matches (caller should return 401).
pub fn identify_consumer<'a>(cfg: &'a ConsumersConfig, session: &Session) -> Option<&'a Consumer> {
    let api_key_header = cfg.api_key_header.as_deref().unwrap_or("x-api-key");

    for consumer in &cfg.consumers {
        // ── API key check ─────────────────────────────────────────────────
        if let Some(ref expected_key) = consumer.api_key {
            if let Some(provided) = session
                .req_header()
                .headers
                .get(api_key_header)
                .and_then(|v| v.to_str().ok())
            {
                if provided == expected_key.as_str() {
                    return Some(consumer);
                }
            }
        }

        // ── Basic Auth check ──────────────────────────────────────────────
        if let Some(ref basic) = consumer.basic_auth {
            if check_consumer_basic(&consumer.username, &basic.password, session) {
                return Some(consumer);
            }
        }
    }
    None
}

/// Validate `Authorization: Basic <b64>` against a consumer's username and password.
fn check_consumer_basic(username: &str, password: &str, session: &Session) -> bool {
    let auth_header = session
        .req_header()
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let encoded = auth_header.strip_prefix("Basic ").unwrap_or("").trim();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok();
    let decoded = match decoded {
        Some(d) => d,
        None => return false,
    };
    let decoded_str = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Format: "username:password" — split only on the first colon.
    let (provided_user, provided_pass) = match decoded_str.split_once(':') {
        Some(pair) => pair,
        None => return false,
    };

    provided_user == username && provided_pass == password
}
