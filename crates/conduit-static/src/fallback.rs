//! Fallback-response handler (404, SPA shell, custom body).
//!
//! Moved from `src/handler/fallback.rs` (issue #114/#139) — see this crate's
//! `src/lib.rs` doc comment.

use async_trait::async_trait;
use bytes::Bytes;
#[cfg(feature = "compression")]
use conduit_compression::logic::CompressOptions;
use conduit_core::handler::response::write_response;
use conduit_core::handler::LocalHandlerImpl;
#[cfg(feature = "compression")]
use conduit_core::util::encoding::AcceptEncoding;
use pingora_core::Result;
use pingora_proxy::Session;
use tokio::fs;

use crate::config::{FallbackConfig, FallbackRule};

/// Handler struct for fallback responses (404, SPA shell, custom body).
pub struct FallbackHandler {
    /// The `fallback` rule to serve; `None` → plain 404.
    pub fallback: Option<FallbackConfig>,
    pub extra_headers: Vec<(String, String)>,
    /// Resolved on-the-fly compression options for this site, if the
    /// `compression` feature is compiled in and enabled — see
    /// `conduit_compression`'s doc comment.
    #[cfg(feature = "compression")]
    pub compress_opts: Option<CompressOptions>,
    #[cfg(feature = "compression")]
    pub accept_enc: AcceptEncoding,
}

#[async_trait]
impl LocalHandlerImpl for FallbackHandler {
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        handle_fallback(
            session,
            self.fallback.as_ref(),
            &self.extra_headers,
            #[cfg(feature = "compression")]
            self.compress_opts.as_ref().map(|o| (o, &self.accept_enc)),
            #[cfg(not(feature = "compression"))]
            None,
        )
        .await
    }
}

pub async fn handle_fallback(
    session: &mut Session,
    fallback: Option<&FallbackConfig>,
    extra: &[(String, String)],
    #[cfg(feature = "compression")] compress: Option<(&CompressOptions, &AcceptEncoding)>,
    #[cfg(not(feature = "compression"))] _compress: Option<()>,
) -> Result<()> {
    if let Some(fb) = fallback {
        return handle_configured(
            session,
            fb,
            extra,
            #[cfg(feature = "compression")]
            compress,
            #[cfg(not(feature = "compression"))]
            _compress,
        )
        .await;
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
    #[cfg(feature = "compression")] compress: Option<(&CompressOptions, &AcceptEncoding)>,
    #[cfg(not(feature = "compression"))] _compress: Option<()>,
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
            return handle_rule(
                session,
                rule,
                extra,
                #[cfg(feature = "compression")]
                compress,
                #[cfg(not(feature = "compression"))]
                _compress,
            )
            .await;
        }
    }

    // ── Flat FallbackConfig (no byAccept, or no match found) ─────────────────
    let rule = FallbackRule {
        status: fb.status,
        body: fb.body.clone(),
        file: fb.file.clone(),
        headers: fb.headers.clone(),
    };
    handle_rule(
        session,
        &rule,
        extra,
        #[cfg(feature = "compression")]
        compress,
        #[cfg(not(feature = "compression"))]
        _compress,
    )
    .await
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

/// Apply on-the-fly compression (when enabled) and write the response —
/// `write_response` plus the negotiation step `compress_bytes`'s own doc
/// comment promises for fallback bodies.
async fn write_maybe_compressed(
    session: &mut Session,
    status: u16,
    content_type: &str,
    body: Bytes,
    extra: &[(String, String)],
    #[cfg(feature = "compression")] compress: Option<(&CompressOptions, &AcceptEncoding)>,
    #[cfg(not(feature = "compression"))] _compress: Option<()>,
) -> Result<()> {
    #[cfg(feature = "compression")]
    let (body, enc_header) =
        conduit_compression::logic::compress_small_body(body, content_type, compress).await;
    #[cfg(not(feature = "compression"))]
    let enc_header: Option<(String, String)> = None;

    if let Some(pair) = enc_header {
        let mut all_extra = extra.to_vec();
        all_extra.push(pair);
        // Response representation depends on Accept-Encoding — same
        // convention as the static-file compression path.
        all_extra.push(("vary".to_owned(), "accept-encoding".to_owned()));
        return write_response(session, status, content_type, body, &all_extra).await;
    }
    write_response(session, status, content_type, body, extra).await
}

/// Serve a response according to a single `FallbackRule`.
async fn handle_rule(
    session: &mut Session,
    rule: &FallbackRule,
    extra: &[(String, String)],
    #[cfg(feature = "compression")] compress: Option<(&CompressOptions, &AcceptEncoding)>,
    #[cfg(not(feature = "compression"))] _compress: Option<()>,
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
                write_maybe_compressed(
                    session,
                    status,
                    ct,
                    Bytes::from(bytes),
                    extra,
                    #[cfg(feature = "compression")]
                    compress,
                    #[cfg(not(feature = "compression"))]
                    _compress,
                )
                .await
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
        write_maybe_compressed(
            session,
            status,
            "application/json",
            Bytes::from(json),
            extra,
            #[cfg(feature = "compression")]
            compress,
            #[cfg(not(feature = "compression"))]
            _compress,
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
