//! Static-file serving handler.
//!
//! Moved from `src/handler/static_files.rs` (issue #114/#139) — see this
//! crate's `src/lib.rs` doc comment. Split by concern (issue #622): `resolve` finds and opens the file
//! below a root, `conditional` decides `304`/`206` and builds the validators, `serve` writes the
//! responses, `stream` copies the body. This file keeps the handler type and the request flow.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
#[cfg(feature = "compression")]
use conduit_compression::logic::CompressOptions;
use conduit_core::handler::LocalHandlerImpl;
use conduit_core::util::encoding::AcceptEncoding;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::config::{FallbackConfig, StaticOptions};
use crate::mime;

mod conditional;
mod resolve;
mod serve;
mod stream;
#[cfg(test)]
mod tests;

use conditional::{if_range_allows_range, is_not_modified, make_cache_control, Validators};
use resolve::{
    decode_rel_path, find_file, has_dotfile, open_served, resolve_pre_compressed, Opened,
};
use serve::{
    serve_full, serve_pre_compressed, serve_range, write_error, write_not_modified, RespCtx,
};

/// Handler struct for static file responses.
///
/// Attempts to serve the file from `roots`; on a miss, delegates to the site's
/// configured fallback (e.g. SPA index.html for HTML requests, 404 for others).
pub struct StaticFileHandler {
    pub roots: Vec<PathBuf>,
    pub options: Arc<StaticOptions>,
    pub strip_prefix: Option<String>,
    pub extra_headers: Vec<(String, String)>,
    /// Resolved on-the-fly compression options for this route, if the
    /// `compression` feature is compiled in and the site has compression
    /// enabled. `#[cfg(feature = "compression")]`-gated like `CompressOptions`
    /// itself — see `conduit_compression`'s doc comment.
    #[cfg(feature = "compression")]
    pub compress_opts: Option<CompressOptions>,
    pub accept_enc: AcceptEncoding,
    /// Fallback rule to serve when the file is not found.
    pub fallback: Option<FallbackConfig>,
}

#[async_trait]
impl LocalHandlerImpl for StaticFileHandler {
    /// Serve the requested file, or hand a miss to the site's fallback (SPA index, 404 page).
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        let found = handle_static(session, self).await?;

        if !found {
            crate::fallback::handle_fallback(
                session,
                self.fallback.as_ref(),
                &self.extra_headers,
                #[cfg(feature = "compression")]
                self.compress_opts.as_ref().map(|o| (o, &self.accept_enc)),
                #[cfg(not(feature = "compression"))]
                None,
            )
            .await?;
        }
        Ok(())
    }
}

/// Attempt to serve a static file.
///
/// Returns `Ok(true)` when a response has been written (success, 304, 403, 206,
/// …).  Returns `Ok(false)` when the file was not found and **no response has
/// been written yet** — the caller must handle the miss (e.g. invoke the
/// fallback handler).
///
/// Everything that does not depend on the request — the roots, the options, the prefix to strip,
/// the extra headers and the compression settings — is read from `handler` (it was seven loose
/// parameters before, one over Sonar's `rust:S107` limit).
pub async fn handle_static(session: &mut Session, handler: &StaticFileHandler) -> Result<bool> {
    let roots = handler.roots.as_slice();
    let options = &handler.options;
    let strip_prefix = handler.strip_prefix.as_deref();
    let extra = handler.extra_headers.as_slice();
    #[cfg(feature = "compression")]
    let compress_opts = handler.compress_opts.as_ref();
    let accept_enc = &handler.accept_enc;
    let method = session.req_header().method.clone();
    let req_path = session.req_header().uri.path().to_owned();
    let rel = decode_rel_path(&req_path, strip_prefix);

    let dot_policy = options.dot_files.as_deref().unwrap_or("ignore");
    if has_dotfile(&rel) {
        match dot_policy {
            "allow" => {} // fall through and serve the file normally
            "deny" => {
                write_error(session, 403, "Forbidden", extra).await?;
                return Ok(true);
            }
            _ => return Ok(false), // "ignore": treat as not found, let fallback handle it
        }
    }

    let Some((root, file_path)) = find_file(roots, &rel, options).await else {
        return Ok(false);
    };

    // Open the file once, here, before anything is written (issue #618). `find_file` only looked at
    // the path; between that and now another process may have swapped the file for a link, replaced
    // it or removed it. The open below does not follow a link at any level, and the size and
    // modification time that go into `Content-Length` and the validators are read from this same
    // handle, which the body is streamed from later, so none of them can describe another file and a
    // refused open is answered before the first header instead of cutting a `200` off.
    let Opened {
        file,
        size: file_size,
        mtime,
    } = match open_served(root, &file_path).await {
        Ok(Some(opened)) => opened,
        Ok(None) => return Ok(false),
        Err(e) => {
            // Not a miss (permission denied, descriptors exhausted, I/O error), and still nothing
            // written: a clean error response.
            let (status, msg) = if e.kind() == std::io::ErrorKind::PermissionDenied {
                (403, "Forbidden")
            } else {
                (500, "Internal Server Error")
            };
            tracing::warn!(path = ?file_path, error = %e, status, "static file could not be opened");
            write_error(session, status, msg, extra).await?;
            return Ok(true);
        }
    };

    let mtime_secs = mtime
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let etag = format!("\"{mtime_secs:x}-{file_size:x}\"");
    let last_modified = httpdate::fmt_http_date(mtime);
    #[cfg(feature = "compression")]
    let can_compress = compress_opts.is_some();
    #[cfg(not(feature = "compression"))]
    let can_compress = false;
    let validators = Validators {
        etag: options.etag.unwrap_or(true).then_some(etag.as_str()),
        last_modified: options
            .last_modified
            .unwrap_or(true)
            .then_some(last_modified.as_str()),
        vary_encoding: can_compress || options.pre_compressed.unwrap_or(false),
    };
    let cache_control = make_cache_control(options);
    let content_type = mime::content_type(&file_path).to_string();

    let hdrs = session.req_header().headers.clone();

    if is_not_modified(
        &hdrs,
        validators.etag,
        validators.last_modified.is_some(),
        mtime,
    ) {
        write_not_modified(session, &validators, &cache_control, extra).await?;
        return Ok(true);
    }

    let is_head = method.as_str() == "HEAD";

    // Range requests bypass compression — byte ranges are incompatible with
    // Content-Encoding transforms.
    // `If-Range` (RFC 9110 §13.1.5): a stale validator turns the range request into a plain 200 with
    // the whole current file, so a resumed download never splices bytes of two different versions.
    let range_hdr = hdrs
        .get("range")
        .and_then(|v| v.to_str().ok())
        .filter(|_| if_range_allows_range(&hdrs, validators.etag, validators.last_modified));
    let ctx = RespCtx {
        content_type: &content_type,
        validators: &validators,
        cache_control: &cache_control,
        is_head,
        extra,
    };
    if let Some(range_hdr) = range_hdr {
        serve_range(session, &ctx, file, file_size, range_hdr).await?;
        return Ok(true);
    }

    // Pre-compressed files: serve `.br` / `.gz` sibling files when available
    // and the client accepts the encoding.  This avoids CPU-intensive on-the-fly
    // compression for assets that were pre-compressed at build time.
    if options.pre_compressed.unwrap_or(false) {
        if let Some(pre) = resolve_pre_compressed(root, &file_path, accept_enc).await {
            serve_pre_compressed(session, &ctx, pre).await?;
            return Ok(true);
        }
    }

    // Pick an on-the-fly encoding if the config and client both support it.
    // Also check content-type: don't compress binary formats (nginx gzip_types pattern).
    #[cfg(feature = "compression")]
    let compress = compress_opts.and_then(|opts| {
        if !conduit_compression::logic::is_compressible_type(&content_type, opts) {
            return None;
        }
        conduit_compression::logic::best_encoding(opts, accept_enc, file_size)
            .map(|enc| (enc, opts.level))
    });
    // `compression` not compiled in: never claim an encoding we can't
    // actually produce (see `stream_file_compressed`'s matching fallback
    // below).
    #[cfg(not(feature = "compression"))]
    let compress: Option<(&'static str, u8)> = None;

    serve_full(session, &ctx, file, file_size, compress).await?;
    Ok(true)
}
