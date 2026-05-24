use bytes::Bytes;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;

use crate::config::schema::{CorsConfig, CorsOptions};

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
pub async fn handle_preflight(
    session: &mut Session,
    cfg: &CorsConfig,
    origin: &str,
    extra: &[(String, String)],
) -> Result<()> {
    let cors_hdrs = preflight_headers(cfg, origin);
    let n = 1 + cors_hdrs.len() + extra.len();
    let mut resp = ResponseHeader::build(204, Some(n))?;
    resp.insert_header("content-length", "0")?;
    for (name, value) in &cors_hdrs {
        resp.insert_header(name.clone(), value.clone())?;
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
    use crate::config::schema::CorsOptions;

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
}
