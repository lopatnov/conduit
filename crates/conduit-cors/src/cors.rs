use bytes::Bytes;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;

use crate::config::{CorsConfig, CorsOptions};

const DEFAULT_METHODS: &str = "GET, POST, PUT, DELETE, PATCH, OPTIONS";
const DEFAULT_HEADERS: &str = "Content-Type, Authorization";

/// Returns `true` when this request is a CORS preflight.
///
/// A preflight is an OPTIONS request that carries both an `Origin` header and
/// an `Access-Control-Request-Method` header.
pub fn is_preflight(session: &Session) -> bool {
    let headers = &session.req_header().headers;
    session.req_header().method.as_str() == "OPTIONS"
        && headers.contains_key("origin")
        && headers.contains_key("access-control-request-method")
}

/// Returns `true` when the preflight requests Private Network Access.
///
/// Chrome's Private Network Access (PNA) spec requires servers to explicitly
/// opt in to requests from public origins targeting private (RFC 1918)
/// endpoints by echoing `Access-Control-Allow-Private-Network: true` back.
/// Pattern from Envoy's CORS filter `access_control_request_private_network`.
pub fn requests_private_network_access(session: &Session) -> bool {
    session
        .req_header()
        .headers
        .get("access-control-request-private-network")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Extract the `Origin` header value.
pub fn request_origin(session: &Session) -> Option<String> {
    session
        .req_header()
        .headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// CORS headers to add to every **non-preflight** response when CORS is enabled.
///
/// Returns an empty vec when CORS is disabled or the request origin is not allowed.
pub fn response_headers(cfg: &CorsConfig, origin: Option<&str>) -> Vec<(String, String)> {
    let Some(origin) = origin else {
        return vec![];
    };
    match cfg {
        CorsConfig::Enabled(false) => vec![],
        CorsConfig::Enabled(true) => build_response_headers(None, origin),
        CorsConfig::Options(opts) => {
            if !is_origin_allowed(opts, origin) {
                return vec![];
            }
            build_response_headers(Some(opts), origin)
        }
    }
}

/// Send a 204 No Content preflight response with CORS + any extra headers.
///
/// `extra` should contain security headers; the CORS preflight headers are
/// computed here from `cfg` and `origin`.
///
/// When `allow_private_network` is `true` (the request sent
/// `Access-Control-Request-Private-Network: true`), the response includes
/// `Access-Control-Allow-Private-Network: true` per the Chrome PNA spec.
/// Pattern from Envoy's CORS filter.
pub async fn handle_preflight(
    session: &mut Session,
    cfg: &CorsConfig,
    origin: &str,
    extra: &[(String, String)],
    allow_private_network: bool,
) -> Result<()> {
    let cors_hdrs = preflight_headers(cfg, origin);
    let pna_extra = if allow_private_network { 1 } else { 0 };
    let n = 1 + cors_hdrs.len() + extra.len() + pna_extra;
    let mut resp = ResponseHeader::build(204, Some(n))?;
    resp.insert_header("content-length", "0")?;
    for (name, value) in &cors_hdrs {
        resp.insert_header(name.clone(), value.clone())?;
    }
    // Chrome PNA: echo opt-in header when the browser requests it.
    if allow_private_network {
        resp.insert_header("access-control-allow-private-network", "true")?;
    }
    for (name, value) in extra {
        resp.insert_header(name.clone(), value.clone())?;
    }
    session.write_response_header(Box::new(resp), false).await?;
    session.write_response_body(Some(Bytes::new()), true).await
}

// ── Internals ─────────────────────────────────────────────────────────────

fn is_origin_allowed(opts: &CorsOptions, origin: &str) -> bool {
    match &opts.origins {
        None => true,
        Some(list) => list.iter().any(|o| o == "*" || o == origin),
    }
}

/// CORS headers for regular (non-preflight) responses.
fn build_response_headers(opts: Option<&CorsOptions>, origin: &str) -> Vec<(String, String)> {
    let credentials = opts.and_then(|o| o.credentials).unwrap_or(false);
    let specific_origins = opts.and_then(|o| o.origins.as_ref()).is_some();

    let mut h = Vec::new();

    // When credentials are required or a specific origin whitelist is configured,
    // echo the request origin back instead of using a wildcard.
    // Wildcard + credentials is rejected by browsers.
    if credentials || specific_origins {
        h.push(("Access-Control-Allow-Origin".to_owned(), origin.to_owned()));
        h.push(("Vary".to_owned(), "Origin".to_owned()));
    } else {
        h.push(("Access-Control-Allow-Origin".to_owned(), "*".to_owned()));
    }

    let methods = opts
        .and_then(|o| o.methods.as_deref())
        .map(|m| m.join(", "))
        .unwrap_or_else(|| DEFAULT_METHODS.to_owned());
    h.push(("Access-Control-Allow-Methods".to_owned(), methods));

    let allowed_headers = opts
        .and_then(|o| o.allowed_headers.as_deref())
        .map(|v| v.join(", "))
        .unwrap_or_else(|| DEFAULT_HEADERS.to_owned());
    h.push(("Access-Control-Allow-Headers".to_owned(), allowed_headers));

    if credentials {
        h.push((
            "Access-Control-Allow-Credentials".to_owned(),
            "true".to_owned(),
        ));
    }

    h
}

/// CORS headers for preflight (OPTIONS) responses — includes `Access-Control-Max-Age`.
fn preflight_headers(cfg: &CorsConfig, origin: &str) -> Vec<(String, String)> {
    match cfg {
        CorsConfig::Enabled(false) => vec![],
        CorsConfig::Enabled(true) => {
            let mut h = build_response_headers(None, origin);
            h.push(("Access-Control-Max-Age".to_owned(), "86400".to_owned()));
            h
        }
        CorsConfig::Options(opts) => {
            if !is_origin_allowed(opts, origin) {
                return vec![];
            }
            let mut h = build_response_headers(Some(opts), origin);
            let max_age = opts.max_age_secs.unwrap_or(86400);
            h.push(("Access-Control-Max-Age".to_owned(), max_age.to_string()));
            h
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CorsOptions;

    fn any_origin_cfg() -> CorsConfig {
        CorsConfig::Enabled(true)
    }

    fn specific_origin_cfg(origin: &str) -> CorsConfig {
        CorsConfig::Options(CorsOptions {
            origins: Some(vec![origin.to_owned()]),
            methods: None,
            allowed_headers: None,
            credentials: None,
            max_age_secs: None,
        })
    }

    #[test]
    fn cors_disabled_returns_empty() {
        let h = response_headers(&CorsConfig::Enabled(false), Some("https://a.com"));
        assert!(h.is_empty());
    }

    #[test]
    fn cors_enabled_wildcard_no_origin() {
        let h = response_headers(&any_origin_cfg(), None);
        assert!(h.is_empty(), "no Origin header → no CORS headers");
    }

    #[test]
    fn cors_enabled_wildcard_with_origin() {
        let h = response_headers(&any_origin_cfg(), Some("https://a.com"));
        let allow_origin = h.iter().find(|(k, _)| k == "Access-Control-Allow-Origin");
        assert_eq!(allow_origin.map(|(_, v)| v.as_str()), Some("*"));
    }

    #[test]
    fn cors_specific_origin_allowed() {
        let cfg = specific_origin_cfg("https://allowed.com");
        let h = response_headers(&cfg, Some("https://allowed.com"));
        let acao = h.iter().find(|(k, _)| k == "Access-Control-Allow-Origin");
        assert_eq!(acao.map(|(_, v)| v.as_str()), Some("https://allowed.com"));
        // Should also set Vary: Origin
        assert!(h.iter().any(|(k, v)| k == "Vary" && v == "Origin"));
    }

    #[test]
    fn cors_specific_origin_rejected() {
        let cfg = specific_origin_cfg("https://allowed.com");
        let h = response_headers(&cfg, Some("https://other.com"));
        assert!(h.is_empty(), "non-listed origin must get no CORS headers");
    }

    #[test]
    fn preflight_includes_max_age() {
        let h = preflight_headers(&any_origin_cfg(), "https://a.com");
        assert!(h.iter().any(|(k, _)| k == "Access-Control-Max-Age"));
    }

    #[test]
    fn preflight_disabled_returns_empty() {
        let h = preflight_headers(&CorsConfig::Enabled(false), "https://a.com");
        assert!(h.is_empty(), "disabled CORS → no preflight headers");
    }

    #[test]
    fn preflight_options_allowed_origin_includes_max_age() {
        let cfg = specific_origin_cfg("https://allowed.com");
        let h = preflight_headers(&cfg, "https://allowed.com");
        assert!(
            h.iter().any(|(k, _)| k == "Access-Control-Max-Age"),
            "allowed origin must include max-age"
        );
        let acao = h.iter().find(|(k, _)| k == "Access-Control-Allow-Origin");
        assert_eq!(
            acao.map(|(_, v)| v.as_str()),
            Some("https://allowed.com"),
            "echo origin for specific-origin configs"
        );
    }

    #[test]
    fn preflight_options_custom_max_age() {
        let cfg = CorsConfig::Options(CorsOptions {
            origins: Some(vec!["https://a.com".to_owned()]),
            methods: None,
            allowed_headers: None,
            credentials: None,
            max_age_secs: Some(3600),
        });
        let h = preflight_headers(&cfg, "https://a.com");
        let max_age = h.iter().find(|(k, _)| k == "Access-Control-Max-Age");
        assert_eq!(max_age.map(|(_, v)| v.as_str()), Some("3600"));
    }

    #[test]
    fn preflight_options_rejected_origin_returns_empty() {
        let cfg = specific_origin_cfg("https://allowed.com");
        let h = preflight_headers(&cfg, "https://other.com");
        assert!(h.is_empty(), "rejected origin → no preflight headers");
    }

    #[test]
    fn response_headers_with_credentials() {
        let cfg = CorsConfig::Options(CorsOptions {
            origins: None,
            methods: None,
            allowed_headers: None,
            credentials: Some(true),
            max_age_secs: None,
        });
        let h = response_headers(&cfg, Some("https://a.com"));
        let cred = h
            .iter()
            .find(|(k, _)| k == "Access-Control-Allow-Credentials");
        assert_eq!(
            cred.map(|(_, v)| v.as_str()),
            Some("true"),
            "credentials:true must emit Allow-Credentials header"
        );
        // Origin should be echoed (not wildcard) when credentials is true.
        let acao = h.iter().find(|(k, _)| k == "Access-Control-Allow-Origin");
        assert_eq!(acao.map(|(_, v)| v.as_str()), Some("https://a.com"));
    }

    #[test]
    fn wildcard_origin_no_credentials_header() {
        let h = response_headers(&any_origin_cfg(), Some("https://a.com"));
        assert!(
            !h.iter()
                .any(|(k, _)| k == "Access-Control-Allow-Credentials"),
            "wildcard CORS must NOT emit credentials header"
        );
    }

    // ── is_origin_allowed ─────────────────────────────────────────────────────

    #[test]
    fn is_origin_allowed_wildcard_entry_in_list() {
        // An origins list containing "*" allows any origin.
        let opts = CorsOptions {
            origins: Some(vec!["*".to_owned()]),
            methods: None,
            allowed_headers: None,
            credentials: None,
            max_age_secs: None,
        };
        assert!(is_origin_allowed(&opts, "https://anything.example.com"));
    }

    #[test]
    fn is_origin_allowed_no_origins_list_allows_all() {
        // `origins: None` means no whitelist — every origin is allowed.
        let opts = CorsOptions {
            origins: None,
            methods: None,
            allowed_headers: None,
            credentials: None,
            max_age_secs: None,
        };
        assert!(is_origin_allowed(&opts, "https://any.example.com"));
    }

    #[test]
    fn is_origin_allowed_specific_list_rejects_unknown() {
        let opts = CorsOptions {
            origins: Some(vec!["https://a.com".to_owned(), "https://b.com".to_owned()]),
            methods: None,
            allowed_headers: None,
            credentials: None,
            max_age_secs: None,
        };
        assert!(!is_origin_allowed(&opts, "https://c.com"));
        assert!(is_origin_allowed(&opts, "https://a.com"));
        assert!(is_origin_allowed(&opts, "https://b.com"));
    }

    // ── build_response_headers with custom methods / allowed_headers ──────────

    #[test]
    fn response_headers_custom_methods() {
        let cfg = CorsConfig::Options(CorsOptions {
            origins: None,
            methods: Some(vec!["GET".to_owned(), "POST".to_owned()]),
            allowed_headers: None,
            credentials: None,
            max_age_secs: None,
        });
        let h = response_headers(&cfg, Some("https://a.com"));
        let methods = h
            .iter()
            .find(|(k, _)| k == "Access-Control-Allow-Methods")
            .map(|(_, v)| v.as_str());
        assert_eq!(methods, Some("GET, POST"), "custom methods must be echoed");
    }

    #[test]
    fn response_headers_custom_allowed_headers() {
        let cfg = CorsConfig::Options(CorsOptions {
            origins: None,
            methods: None,
            allowed_headers: Some(vec![
                "X-Custom-Header".to_owned(),
                "Authorization".to_owned(),
            ]),
            credentials: None,
            max_age_secs: None,
        });
        let h = response_headers(&cfg, Some("https://a.com"));
        let hdrs = h
            .iter()
            .find(|(k, _)| k == "Access-Control-Allow-Headers")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            hdrs,
            Some("X-Custom-Header, Authorization"),
            "custom allowed headers must be echoed"
        );
    }

    #[test]
    fn response_headers_default_methods_when_none_configured() {
        // When opts.methods is None, the DEFAULT_METHODS constant should be used.
        let cfg = CorsConfig::Options(CorsOptions {
            origins: None,
            methods: None,
            allowed_headers: None,
            credentials: None,
            max_age_secs: None,
        });
        let h = response_headers(&cfg, Some("https://a.com"));
        let methods = h
            .iter()
            .find(|(k, _)| k == "Access-Control-Allow-Methods")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            methods,
            Some(DEFAULT_METHODS),
            "should fall back to DEFAULT_METHODS"
        );
    }

    #[test]
    fn response_headers_default_headers_when_none_configured() {
        let cfg = CorsConfig::Options(CorsOptions {
            origins: None,
            methods: None,
            allowed_headers: None,
            credentials: None,
            max_age_secs: None,
        });
        let h = response_headers(&cfg, Some("https://a.com"));
        let hdrs = h
            .iter()
            .find(|(k, _)| k == "Access-Control-Allow-Headers")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            hdrs,
            Some(DEFAULT_HEADERS),
            "should fall back to DEFAULT_HEADERS"
        );
    }

    #[test]
    fn preflight_options_no_origins_wildcard_includes_max_age_default() {
        // CorsConfig::Options with origins=None and no max_age_secs → 86400
        let cfg = CorsConfig::Options(CorsOptions {
            origins: None,
            methods: None,
            allowed_headers: None,
            credentials: None,
            max_age_secs: None,
        });
        let h = preflight_headers(&cfg, "https://any.com");
        let max_age = h
            .iter()
            .find(|(k, _)| k == "Access-Control-Max-Age")
            .map(|(_, v)| v.as_str());
        assert_eq!(max_age, Some("86400"), "default max-age must be 86400");
    }
}
