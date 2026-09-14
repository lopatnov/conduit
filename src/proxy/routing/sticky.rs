//! Sticky-session resolution + HMAC helpers (issue #143, PR A2 of a 3-PR
//! plan) — moved verbatim out of `router.rs`.

use crate::config::schema::{LoadBalanceStrategy, StickyConfig};
use crate::proxy::routing::options::ProxyCtx;

/// Outcome of evaluating the sticky-session cookie.
pub(crate) enum Sticky {
    /// No sticky config, no cookie, or an unverifiable cookie in HMAC mode —
    /// use the configured load-balancing strategy.
    None,
    /// HMAC mode: the cookie verified against this exact upstream URL.
    ///
    /// This is a **routing target**, not a hash input (#220). Honoring it
    /// means routing to it *directly*: hashing the URL string and taking
    /// `% len` lands back on the pinned peer only by coincidence — measured
    /// at ~23% across 2..8-peer rings (i.e. chance), and with 4 upstreams
    /// *never*. Before #220 this variant was conflated with [`Self::HashKey`]
    /// below, so an HMAC-signed session was silently served by a different
    /// peer than the one its cookie names on ~3 of every 4 requests.
    Pinned(String),
    /// Legacy no-secret mode: an opaque, client-supplied cookie value.
    ///
    /// There is no pinned URL to honor here — the value is only ever usable
    /// as a consistent-hash key, which is correct for this mode (any stable
    /// string maps to a stable peer).
    HashKey(String),
    /// `sticky.strict` and the pinned peer is unhealthy — refuse with 503.
    Reject,
}

/// When `sticky.secret` is set, verify the HMAC-SHA256 of each candidate
/// upstream URL to find the pinned backend. A forged or unmatched cookie
/// falls through to normal load-balancing (or returns `Reject` in strict
/// mode). Without `secret`, the raw cookie value is used as the
/// consistent-hash key (legacy behavior).
///
/// Security: a cookie that fails signature verification must NOT influence
/// routing — otherwise a client could forge/manipulate their cookie to
/// steer to specific upstreams. Raw cookie values are only used when no
/// secret is set (legacy, non-HMAC sticky).
pub(crate) fn resolve_sticky(
    sticky: Option<&StickyConfig>,
    all_urls: &[String],
    ctx: &ProxyCtx<'_>,
) -> Sticky {
    let Some(cfg) = sticky else {
        return Sticky::None;
    };
    let Some(cookie_val) = extract_cookie(ctx.req_headers, &cfg.cookie) else {
        return Sticky::None;
    };
    let Some(secret) = cfg.secret.as_deref() else {
        // No secret configured: use raw cookie as consistent-hash input.
        return Sticky::HashKey(cookie_val);
    };
    // Try to find the upstream whose HMAC matches the cookie.
    let Some(pinned) = all_urls
        .iter()
        .find(|u| hmac_verify_sticky(u, &cookie_val, secret))
    else {
        // HMAC mode but cookie failed verification: ignore — fall through
        // to the configured load-balancing strategy.
        return Sticky::None;
    };
    // Strict mode: if the client presented a signed cookie for a peer that
    // is now unhealthy, refuse the request rather than silently routing to
    // a different upstream (which would break session affinity).
    if cfg.strict.unwrap_or(false) && !ctx.upstream_health.is_healthy(pinned) {
        tracing::debug!(
            url = %pinned,
            "sticky strict mode: pinned upstream unhealthy — returning 503"
        );
        return Sticky::Reject;
    }
    Sticky::Pinned(pinned.clone())
}

/// Hash key for ip-hash / consistent-hash / sticky selection.
/// Priority: sticky cookie > `hashKey: "url"` (or empty client IP) > client IP.
pub(crate) fn selection_hash_val(
    sticky_override: Option<&str>,
    hash_key: &str,
    path: &str,
    client_ip: &str,
) -> u64 {
    let hash_input = if let Some(cookie_val) = sticky_override {
        cookie_val
    } else if hash_key == "url" || client_ip.is_empty() {
        path
    } else {
        client_ip
    };
    crate::proxy::upstream::fnv1a_hash(hash_input)
}

/// Sticky sessions always select by consistent hash of the cookie value.
static STICKY_STRATEGY: LoadBalanceStrategy = LoadBalanceStrategy::ConsistentHash;

pub(crate) fn effective_strategy(
    sticky_active: bool,
    configured: Option<&LoadBalanceStrategy>,
) -> Option<&LoadBalanceStrategy> {
    if sticky_active {
        Some(&STICKY_STRATEGY)
    } else {
        configured
    }
}

/// HMAC-signed sticky cookie to set on the response, when `sticky.secret` is set.
pub(crate) fn make_sticky_cookie(
    sticky: Option<&StickyConfig>,
    chosen_url: &str,
) -> Option<(String, String)> {
    let cfg = sticky?;
    let secret = cfg.secret.as_deref()?;
    let signed = hmac_sign_sticky(chosen_url, secret);
    Some((cfg.cookie.clone(), signed))
}

/// Extract the value of a named cookie from the `Cookie` request header.
///
/// Returns `None` when the cookie is absent or the header cannot be parsed.
pub(crate) fn extract_cookie(headers: &http::HeaderMap, name: &str) -> Option<String> {
    let cookie_hdr = headers.get("cookie")?.to_str().ok()?;
    for pair in cookie_hdr.split(';') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            if k.trim() == name {
                return Some(v.trim().to_owned());
            }
        }
    }
    None
}

// ── Sticky-session HMAC helpers ───────────────────────────────────────────────

/// Compute `HMAC-SHA256(upstream_url, secret)` and return it as URL-safe base64
/// (no padding).  Used for both signing response cookies and verifying requests.
pub(crate) fn hmac_sign_sticky(upstream_url: &str, secret: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(upstream_url.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// Return `true` when `cookie_value` is the valid HMAC of `upstream_url` with
/// the given `secret`.  Uses constant-time comparison to prevent timing attacks.
pub(crate) fn hmac_verify_sticky(upstream_url: &str, cookie_value: &str, secret: &str) -> bool {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use subtle::ConstantTimeEq as _;

    let expected = hmac_sign_sticky(upstream_url, secret);
    let Ok(expected_bytes) = URL_SAFE_NO_PAD.decode(&expected) else {
        return false;
    };
    let Ok(actual_bytes) = URL_SAFE_NO_PAD.decode(cookie_value) else {
        return false;
    };
    expected_bytes.len() == actual_bytes.len() && expected_bytes.ct_eq(&actual_bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_cookie ────────────────────────────────────────────────────────

    #[test]
    fn extract_cookie_found() {
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert("cookie", "session=abc123; lang=en".parse().unwrap());
        assert_eq!(extract_cookie(&hdrs, "session"), Some("abc123".to_owned()));
    }

    #[test]
    fn extract_cookie_second_value() {
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert("cookie", "a=1; b=2; c=3".parse().unwrap());
        assert_eq!(extract_cookie(&hdrs, "b"), Some("2".to_owned()));
        assert_eq!(extract_cookie(&hdrs, "c"), Some("3".to_owned()));
    }

    #[test]
    fn extract_cookie_missing_returns_none() {
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert("cookie", "a=1; b=2".parse().unwrap());
        assert!(extract_cookie(&hdrs, "missing").is_none());
    }

    #[test]
    fn extract_cookie_no_cookie_header_returns_none() {
        let hdrs = http::HeaderMap::new();
        assert!(extract_cookie(&hdrs, "session").is_none());
    }

    #[test]
    fn extract_cookie_strips_whitespace() {
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert("cookie", "key = value ".parse().unwrap());
        assert_eq!(extract_cookie(&hdrs, "key"), Some("value".to_owned()));
    }

    // ── hmac_sign_sticky / hmac_verify_sticky (#39) ───────────────────────────

    #[test]
    fn hmac_sign_sticky_produces_non_empty_base64() {
        let signed = hmac_sign_sticky("http://backend:4000", "mysecret");
        assert!(!signed.is_empty(), "HMAC signature must not be empty");
        // URL-safe base64: only A-Z a-z 0-9 - _ (no + / =)
        assert!(
            signed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "signature must be URL-safe base64 (no +, /, or =): {signed}"
        );
    }

    #[test]
    fn hmac_sign_sticky_is_deterministic() {
        let a = hmac_sign_sticky("http://backend:4000", "s3cret");
        let b = hmac_sign_sticky("http://backend:4000", "s3cret");
        assert_eq!(a, b, "same inputs must always produce the same HMAC");
    }

    #[test]
    fn hmac_sign_sticky_differs_for_different_urls() {
        let a = hmac_sign_sticky("http://backend-a:4000", "s3cret");
        let b = hmac_sign_sticky("http://backend-b:4000", "s3cret");
        assert_ne!(a, b, "different URLs must produce different HMACs");
    }

    #[test]
    fn hmac_sign_sticky_differs_for_different_secrets() {
        let a = hmac_sign_sticky("http://backend:4000", "secret1");
        let b = hmac_sign_sticky("http://backend:4000", "secret2");
        assert_ne!(a, b, "different secrets must produce different HMACs");
    }

    #[test]
    fn hmac_verify_sticky_correct_value_returns_true() {
        let url = "http://backend:4000";
        let secret = "s3cret";
        let signed = hmac_sign_sticky(url, secret);
        assert!(
            hmac_verify_sticky(url, &signed, secret),
            "correct HMAC must verify successfully"
        );
    }

    #[test]
    fn hmac_verify_sticky_wrong_url_returns_false() {
        let secret = "s3cret";
        let signed = hmac_sign_sticky("http://backend-a:4000", secret);
        assert!(
            !hmac_verify_sticky("http://backend-b:4000", &signed, secret),
            "HMAC signed for URL-A must not verify against URL-B"
        );
    }

    #[test]
    fn hmac_verify_sticky_wrong_secret_returns_false() {
        let url = "http://backend:4000";
        let signed = hmac_sign_sticky(url, "correct-secret");
        assert!(
            !hmac_verify_sticky(url, &signed, "wrong-secret"),
            "HMAC signed with one secret must not verify with a different secret"
        );
    }

    #[test]
    fn hmac_verify_sticky_tampered_value_returns_false() {
        let url = "http://backend:4000";
        let secret = "s3cret";
        let mut signed = hmac_sign_sticky(url, secret);
        // Flip the first character to simulate tampering.
        if signed.starts_with('A') {
            signed.replace_range(0..1, "B");
        } else {
            signed.replace_range(0..1, "A");
        }
        assert!(
            !hmac_verify_sticky(url, &signed, secret),
            "tampered cookie value must not verify"
        );
    }

    #[test]
    fn hmac_verify_sticky_garbage_input_returns_false() {
        // Non-base64 input must not panic, just return false.
        assert!(
            !hmac_verify_sticky("http://backend:4000", "not!valid!base64!!!!", "s3cret"),
            "invalid base64 must return false without panicking"
        );
    }
}
