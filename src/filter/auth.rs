use base64::Engine as _;
use pingora_proxy::Session;

use crate::config::schema::{ApiKeyConfig, BasicAuthConfig};

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

    let b64 = match auth_value.strip_prefix("Basic ") {
        Some(b) => b.trim(),
        None => return BasicAuthResult::Denied { challenge, realm },
    };

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

    match cfg.users.get(username) {
        Some(expected) if expected == password => BasicAuthResult::Allowed,
        _ => BasicAuthResult::Denied { challenge, realm },
    }
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
