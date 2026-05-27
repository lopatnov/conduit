use bytes::Bytes;
use pingora_core::Result;
use pingora_proxy::Session;
use tokio::fs;

use crate::config::schema::{FallbackConfig, FallbackRule, SiteConfig};
use crate::handler::response::write_response;

pub async fn handle_fallback(
    session: &mut Session,
    site: Option<&SiteConfig>,
    extra: &[(String, String)],
) -> Result<()> {
    if let Some(site) = site {
        if let Some(fb) = &site.fallback {
            return handle_configured(session, fb, extra).await;
        }
    }
    write_response(
        session,
        404,
        "text/plain",
        Bytes::from_static(b"Not Found"),
        extra,
    )
    .await
}

async fn handle_configured(
    session: &mut Session,
    fb: &FallbackConfig,
    extra: &[(String, String)],
) -> Result<()> {
    // ── Content-negotiation via byAccept ─────────────────────────────────────
    if let Some(ref by_accept) = fb.by_accept {
        let accept = session
            .req_header()
            .headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let rule = pick_by_accept(by_accept, accept);
        if let Some(rule) = rule {
            return handle_rule(session, rule, extra).await;
        }
    }

    // ── Flat FallbackConfig (no byAccept, or no match found) ─────────────────
    let rule = FallbackRule {
        status: fb.status,
        body: fb.body.clone(),
        file: fb.file.clone(),
        headers: fb.headers.clone(),
    };
    handle_rule(session, &rule, extra).await
}

/// Pick the best `FallbackRule` from a `byAccept` map given the request's
/// `Accept` header value.
///
/// Matching order:
/// 1. Check each non-wildcard key (e.g. `"html"`, `"json"`) using
///    [`accept_matches`].
/// 2. Fall back to the wildcard `"*"` entry, if present.
fn pick_by_accept<'a>(
    by_accept: &'a indexmap::IndexMap<String, FallbackRule>,
    accept: &str,
) -> Option<&'a FallbackRule> {
    // First pass: specific keys.
    for (key, rule) in by_accept.iter() {
        if key != "*" && accept_matches(accept, key) {
            return Some(rule);
        }
    }
    // Wildcard fallback.
    by_accept.get("*")
}

/// Return `true` when `accept` contains a content type that matches `key`.
///
/// | key           | matches when `accept` contains         |
/// |---------------|----------------------------------------|
/// | `"html"`      | `text/html`                           |
/// | `"json"`      | `application/json`                    |
/// | `"text"`      | `text/plain` or `text/`               |
/// | `"image"`     | `image/`                              |
/// | any other key | the key as a literal substring        |
fn accept_matches(accept: &str, key: &str) -> bool {
    let patterns: &[&str] = match key {
        "html" => &["text/html"],
        "json" => &["application/json"],
        "text" => &["text/plain", "text/"],
        "image" => &["image/"],
        other => &[other],
    };
    let accept_lc = accept.to_ascii_lowercase();
    patterns.iter().any(|p| accept_lc.contains(p))
}

/// Serve a response according to a single `FallbackRule`.
async fn handle_rule(
    session: &mut Session,
    rule: &FallbackRule,
    extra: &[(String, String)],
) -> Result<()> {
    let status = rule.status.unwrap_or(404);

    // Merge site-level `extra` headers with any rule-specific custom headers.
    // Rule headers are appended last so they can override site-level ones.
    let mut all_headers = extra.to_vec();
    if let Some(ref custom) = rule.headers {
        for (k, v) in custom {
            all_headers.push((k.clone(), v.clone()));
        }
    }
    let extra = all_headers.as_slice();

    // Prefer `file` over `body`.
    if let Some(ref path) = rule.file {
        match fs::read(path).await {
            Ok(bytes) => {
                let ct = mime_guess::from_path(path)
                    .first_raw()
                    .unwrap_or("application/octet-stream");
                write_response(session, status, ct, Bytes::from(bytes), extra).await
            }
            Err(e) => {
                tracing::warn!(path, "fallback file not found: {e}");
                write_response(
                    session,
                    404,
                    "text/plain",
                    Bytes::from_static(b"Not Found"),
                    extra,
                )
                .await
            }
        }
    } else if let Some(ref body) = rule.body {
        let json = body.to_string();
        write_response(
            session,
            status,
            "application/json",
            Bytes::from(json),
            extra,
        )
        .await
    } else {
        write_response(
            session,
            status,
            "text/plain",
            Bytes::from_static(b"Not Found"),
            extra,
        )
        .await
    }
}
