use base64::Engine as _;
use pingora_proxy::Session;

use crate::config::schema::{ApiKeyConfig, BasicAuthConfig};

/// Constant-time byte-string equality for credential comparisons.
///
/// Promoted to `conduit_core::util::crypto::ct_eq_str` (issue #114/#134) so
/// both this file's always-on Basic Auth / API-key guards and
/// `crates/conduit-auth-consumers`' consumer-credential checks share one
/// real implementation. Re-imported under its original short name here so
/// existing call sites in this file don't need to change.
use conduit_core::util::crypto::ct_eq_str;

/// Result of a Basic Auth credential check.
pub enum BasicAuthResult {
    Allowed,
    Denied { challenge: bool, realm: String },
}

// Layer-0 vocabulary (#114/#126) -- shared by every skipPaths config field
// (JWT, ForwardAuth, Consumers).
pub use conduit_core::filter::path::is_path_skipped;

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
        Some(expected) if ct_eq_str(expected, password) => BasicAuthResult::Allowed,
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

    // Compare against ALL keys without early exit — prevents an attacker from
    // inferring the key's position in the list via response-time differences.
    cfg.keys
        .iter()
        .fold(false, |found, k| found | ct_eq_str(k, provided))
}

// ── Consumer model — moved to crates/conduit-auth-consumers (#114/#134) ────
//
// `identify_consumer`/`check_shared_jwt_consumer`/`check_consumer_credentials`/
// `check_consumer_basic`/`build_jwt_auth_cfg` all moved to
// `crates/conduit-auth-consumers/src/identify.rs`. `ConsumersGuard` itself
// (in `src/filter/chain.rs`) now calls `conduit_auth_consumers::identify_consumer`
// directly — see that crate's `src/lib.rs` doc comment for why the guard
// itself didn't move too.

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

    // ── path_matches / is_path_skipped moved to conduit_core::filter::path ─────

    // ── ct_eq_str (timing-safe comparison) — moved to conduit_core::util::crypto,
    // its own unit tests moved with it (issue #114/#134) ────────────────────────

    // ── check_credentials extended cases ─────────────────────────────────────

    #[test]
    fn credentials_empty_user_map_denied() {
        // No users configured → always denied.
        let users = indexmap::IndexMap::new();
        assert!(denied(check_credentials(
            &users,
            "Basic YWRtaW46c2VjcmV0",
            true,
            "R".to_owned()
        )));
    }

    #[test]
    fn credentials_realm_included_in_denial() {
        let u = users(&[("admin", "secret")]);
        let result = check_credentials(&u, "", true, "MyRealm".to_owned());
        if let BasicAuthResult::Denied { realm, .. } = result {
            assert_eq!(realm, "MyRealm");
        } else {
            panic!("expected Denied");
        }
    }

    #[test]
    fn credentials_challenge_false_propagated() {
        let u = users(&[("admin", "secret")]);
        let result = check_credentials(&u, "", false, "R".to_owned());
        if let BasicAuthResult::Denied { challenge, .. } = result {
            assert!(!challenge);
        } else {
            panic!("expected Denied");
        }
    }

    // ── build_jwt_auth_cfg — moved to crates/conduit-auth-consumers (#114/#134) ─
}
