use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_compression::tokio::bufread::{BrotliEncoder, DeflateEncoder, GzipEncoder};
use async_compression::Level;
use bytes::Bytes;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt};

use crate::config::schema::StaticOptions;
use crate::filter::compression::CompressOptions;
use crate::proxy::ctx::AcceptEncoding;
use crate::util::mime;

/// Attempt to serve a static file.
///
/// Returns `Ok(true)` when a response has been written (success, 304, 403, 206,
/// …).  Returns `Ok(false)` when the file was not found and **no response has
/// been written yet** — the caller must handle the miss (e.g. invoke the
/// fallback handler).
pub async fn handle_static(
    session: &mut Session,
    roots: &[PathBuf],
    options: &Arc<StaticOptions>,
    strip_prefix: Option<&str>,
    extra: &[(String, String)],
    compress_opts: Option<&CompressOptions>,
    accept_enc: &AcceptEncoding,
) -> Result<bool> {
    let method = session.req_header().method.clone();
    let req_path = session.req_header().uri.path().to_owned();
    let rel = decode_rel_path(&req_path, strip_prefix);

    let dot_policy = options.dot_files.as_deref().unwrap_or("ignore");
    if has_dotfile(&rel) {
        return match dot_policy {
            "deny" => {
                write_error(session, 403, "Forbidden", extra).await?;
                Ok(true)
            }
            _ => Ok(false), // treat as not-found so fallback can handle it
        };
    }

    let Some(file_path) = find_file(roots, &rel, options).await else {
        return Ok(false);
    };

    let meta = match tokio::fs::metadata(&file_path).await {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };

    let file_size = meta.len();
    let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
    let mtime_secs = mtime
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let etag = format!("\"{mtime_secs:x}-{file_size:x}\"");
    let last_modified = httpdate::fmt_http_date(mtime);
    let cache_control = make_cache_control(options);
    let content_type = mime::content_type(&file_path).to_string();

    let hdrs = session.req_header().headers.clone();

    if is_not_modified(&hdrs, &etag, mtime) {
        write_not_modified(session, &etag, &last_modified, &cache_control, extra).await?;
        return Ok(true);
    }

    let is_head = method.as_str() == "HEAD";

    // Range requests bypass compression — byte ranges are incompatible with
    // Content-Encoding transforms.
    if let Some(range_hdr) = hdrs.get("range").and_then(|v| v.to_str().ok()) {
        serve_range(
            session,
            &file_path,
            file_size,
            range_hdr.to_owned(),
            &content_type,
            &etag,
            &last_modified,
            &cache_control,
            is_head,
            extra,
        )
        .await?;
        return Ok(true);
    }

    // Pre-compressed files: serve `.br` / `.gz` sibling files when available
    // and the client accepts the encoding.  This avoids CPU-intensive on-the-fly
    // compression for assets that were pre-compressed at build time.
    if options.pre_compressed.unwrap_or(false) {
        if let Some((pre_path, encoding)) = find_pre_compressed(&file_path, accept_enc).await {
            let pre_meta = tokio::fs::metadata(&pre_path).await.ok();
            let pre_size = pre_meta.map(|m| m.len()).unwrap_or(0);
            serve_pre_compressed(
                session,
                &pre_path,
                pre_size,
                &content_type,
                encoding,
                &etag,
                &last_modified,
                &cache_control,
                is_head,
                extra,
            )
            .await?;
            return Ok(true);
        }
    }

    // Pick an on-the-fly encoding if the config and client both support it.
    let compress = compress_opts.and_then(|opts| {
        crate::filter::compression::best_encoding(opts, accept_enc, file_size)
            .map(|enc| (enc, opts.level))
    });

    serve_full(
        session,
        &file_path,
        file_size,
        &content_type,
        &etag,
        &last_modified,
        &cache_control,
        is_head,
        extra,
        compress,
    )
    .await?;
    Ok(true)
}

// ── File resolution ────────────────────────────────────────────────────────

async fn find_file(roots: &[PathBuf], rel: &str, options: &StaticOptions) -> Option<PathBuf> {
    for root in roots {
        let candidate = root.join(rel);
        match tokio::fs::metadata(&candidate).await {
            Ok(m) if m.is_file() => return Some(candidate),
            Ok(m) if m.is_dir() => {
                if let Some(p) = find_index(&candidate, options).await {
                    return Some(p);
                }
            }
            _ => {}
        }
    }
    if rel.is_empty() {
        for root in roots {
            if let Some(p) = find_index(root, options).await {
                return Some(p);
            }
        }
    }
    None
}

async fn find_index(dir: &Path, options: &StaticOptions) -> Option<PathBuf> {
    let defaults = vec!["index.html".to_string()];
    let indices = options.index.as_deref().unwrap_or(&defaults);
    for name in indices {
        let p = dir.join(name);
        if tokio::fs::metadata(&p)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

// ── Response helpers ───────────────────────────────────────────────────────

fn insert_extra(resp: &mut ResponseHeader, extra: &[(String, String)]) -> Result<()> {
    for (name, value) in extra {
        resp.insert_header(name.clone(), value.clone())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn serve_full(
    session: &mut Session,
    path: &Path,
    size: u64,
    content_type: &str,
    etag: &str,
    last_modified: &str,
    cache_control: &str,
    is_head: bool,
    extra: &[(String, String)],
    compress: Option<(&'static str, u8)>,
) -> Result<()> {
    if let Some((encoding, level)) = compress {
        // Compressed response — no Content-Length (chunked transfer encoding).
        let header_count = 6 + extra.len(); // no content-length, +2 for encoding+vary
        let mut resp = ResponseHeader::build(200, Some(header_count))?;
        resp.insert_header("content-type", content_type)?;
        resp.insert_header("etag", etag)?;
        resp.insert_header("last-modified", last_modified)?;
        resp.insert_header("cache-control", cache_control)?;
        resp.insert_header("accept-ranges", "bytes")?;
        resp.insert_header("content-encoding", encoding)?;
        resp.insert_header("vary", "accept-encoding")?;
        insert_extra(&mut resp, extra)?;

        if is_head {
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(());
        }

        session.write_response_header(Box::new(resp), false).await?;
        stream_file_compressed(session, path, 0, size, encoding, level).await
    } else {
        let mut resp = ResponseHeader::build(200, Some(6 + extra.len()))?;
        resp.insert_header("content-type", content_type)?;
        resp.insert_header("content-length", size.to_string())?;
        resp.insert_header("etag", etag)?;
        resp.insert_header("last-modified", last_modified)?;
        resp.insert_header("cache-control", cache_control)?;
        resp.insert_header("accept-ranges", "bytes")?;
        insert_extra(&mut resp, extra)?;

        if is_head {
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(());
        }

        session.write_response_header(Box::new(resp), false).await?;
        stream_file(session, path, 0, size).await
    }
}

/// Look for a pre-compressed sibling file next to `path`.
///
/// Preference order: brotli (`.br`) → gzip (`.gz`), filtered by what the
/// client declares in `Accept-Encoding`.  Returns the path to the
/// pre-compressed file and its encoding token (`"br"` or `"gzip"`), or `None`
/// when no suitable sibling exists.
async fn find_pre_compressed(
    path: &Path,
    accept_enc: &AcceptEncoding,
) -> Option<(PathBuf, &'static str)> {
    // Candidates in preference order.
    let candidates: &[(&'static str, &'static str)] =
        &[("br", ".br"), ("gzip", ".gz")];

    for (enc, suffix) in candidates {
        let accept = match *enc {
            "br" => accept_enc.brotli,
            "gzip" => accept_enc.gzip,
            _ => false,
        };
        if !accept {
            continue;
        }
        let mut pre = path.as_os_str().to_owned();
        pre.push(suffix);
        let pre_path = PathBuf::from(pre);
        if tokio::fs::metadata(&pre_path)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return Some((pre_path, enc));
        }
    }
    None
}

/// Serve a pre-compressed file directly — no on-the-fly encoding needed.
///
/// Sends the original resource's `Content-Type`, `ETag`, `Last-Modified`, and
/// `Cache-Control`, while adding `Content-Encoding` and `Vary: accept-encoding`
/// to inform caches that this representation is encoding-specific.
#[allow(clippy::too_many_arguments)]
async fn serve_pre_compressed(
    session: &mut Session,
    pre_path: &Path,
    pre_size: u64,
    content_type: &str,
    encoding: &'static str,
    etag: &str,
    last_modified: &str,
    cache_control: &str,
    is_head: bool,
    extra: &[(String, String)],
) -> Result<()> {
    let mut resp = ResponseHeader::build(200, Some(8 + extra.len()))?;
    resp.insert_header("content-type", content_type)?;
    resp.insert_header("content-length", pre_size.to_string())?;
    resp.insert_header("content-encoding", encoding)?;
    resp.insert_header("vary", "accept-encoding")?;
    resp.insert_header("etag", etag)?;
    resp.insert_header("last-modified", last_modified)?;
    resp.insert_header("cache-control", cache_control)?;
    resp.insert_header("accept-ranges", "none")?; // ranges unsupported on pre-compressed
    insert_extra(&mut resp, extra)?;

    if is_head {
        session.write_response_header(Box::new(resp), true).await?;
        return Ok(());
    }

    session.write_response_header(Box::new(resp), false).await?;
    stream_file(session, pre_path, 0, pre_size).await
}

/// Stream a file compressed with the given encoding and quality level.
///
/// Uses Tokio async encoders so no blocking thread is required.  No
/// Content-Length is sent — the HTTP/1.1 layer uses chunked transfer encoding
/// automatically.
async fn stream_file_compressed(
    session: &mut Session,
    path: &Path,
    offset: u64,
    length: u64,
    encoding: &str,
    level: u8,
) -> Result<()> {
    let mut file = tokio::fs::File::open(path).await.map_err(|e| {
        pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string())
    })?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| {
                pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string())
            })?;
    }

    // Limit the file reader to `length` bytes.
    let limited = file.take(length);
    let buf_reader = tokio::io::BufReader::new(limited);
    let lev = Level::Precise(i32::from(level));

    match encoding {
        "br" => {
            let encoder = BrotliEncoder::with_quality(buf_reader, lev);
            stream_encoded(session, encoder).await
        }
        "gzip" => {
            let encoder = GzipEncoder::with_quality(buf_reader, lev);
            stream_encoded(session, encoder).await
        }
        "deflate" => {
            let encoder = DeflateEncoder::with_quality(buf_reader, lev);
            stream_encoded(session, encoder).await
        }
        _ => {
            // Unknown encoding — fall back to uncompressed streaming.
            stream_file(session, path, offset, length).await
        }
    }
}

/// Drain an `AsyncRead` encoder in 64 KiB chunks, signalling `done=true` on
/// the last write.
///
/// We use a "one-chunk-ahead" pattern: we always buffer the chunk we just read
/// and send it on the *next* iteration, so we know whether there is more data
/// before we call `write_response_body`.  This lets us set `done=true` on the
/// final chunk without reading an extra zero-length chunk first.
async fn stream_encoded<R: AsyncRead + Unpin>(session: &mut Session, mut reader: R) -> Result<()> {
    const CHUNK: usize = 64 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut pending: Option<Bytes> = None;

    loop {
        let n = reader.read(&mut buf).await.map_err(|e| {
            pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string())
        })?;

        if n == 0 {
            // EOF — flush whatever is pending (may be None if the file was empty).
            let chunk = pending.take();
            session.write_response_body(chunk, true).await?;
            return Ok(());
        }

        // Send the previously buffered chunk (not done yet — we have more data).
        if let Some(prev) = pending.take() {
            session.write_response_body(Some(prev), false).await?;
        }

        pending = Some(Bytes::copy_from_slice(&buf[..n]));
    }
}

/// Stream `length` bytes from `file` starting at `offset` in 64 KiB chunks.
async fn stream_file(session: &mut Session, path: &Path, offset: u64, length: u64) -> Result<()> {
    const CHUNK: usize = 64 * 1024;
    let mut file = tokio::fs::File::open(path).await.map_err(|e| {
        pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string())
    })?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| {
                pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string())
            })?;
    }
    let mut remaining = length;
    let mut buf = vec![0u8; CHUNK];
    while remaining > 0 {
        let to_read = (remaining as usize).min(CHUNK);
        let n = file.read(&mut buf[..to_read]).await.map_err(|e| {
            pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string())
        })?;
        if n == 0 {
            break; // Unexpected EOF — client will detect truncation.
        }
        remaining -= n as u64;
        let chunk = Bytes::copy_from_slice(&buf[..n]);
        let done = remaining == 0;
        session.write_response_body(Some(chunk), done).await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn serve_range(
    session: &mut Session,
    path: &Path,
    total: u64,
    range_hdr: String,
    content_type: &str,
    etag: &str,
    last_modified: &str,
    cache_control: &str,
    is_head: bool,
    extra: &[(String, String)],
) -> Result<()> {
    let Some((start, end)) = parse_range(&range_hdr, total) else {
        let mut resp = ResponseHeader::build(416, Some(2 + extra.len()))?;
        resp.insert_header("content-range", format!("bytes */{total}"))?;
        resp.insert_header("content-length", "0")?;
        insert_extra(&mut resp, extra)?;
        session.write_response_header(Box::new(resp), true).await?;
        return Ok(());
    };

    let length = end - start + 1;
    let mut resp = ResponseHeader::build(206, Some(7 + extra.len()))?;
    resp.insert_header("content-type", content_type)?;
    resp.insert_header("content-length", length.to_string())?;
    resp.insert_header("content-range", format!("bytes {start}-{end}/{total}"))?;
    resp.insert_header("etag", etag)?;
    resp.insert_header("last-modified", last_modified)?;
    resp.insert_header("cache-control", cache_control)?;
    resp.insert_header("accept-ranges", "bytes")?;
    insert_extra(&mut resp, extra)?;

    if is_head {
        session.write_response_header(Box::new(resp), true).await?;
        return Ok(());
    }

    session.write_response_header(Box::new(resp), false).await?;
    stream_file(session, path, start, length).await
}

async fn write_not_modified(
    session: &mut Session,
    etag: &str,
    last_modified: &str,
    cache_control: &str,
    extra: &[(String, String)],
) -> Result<()> {
    let mut resp = ResponseHeader::build(304, Some(3 + extra.len()))?;
    resp.insert_header("etag", etag)?;
    resp.insert_header("last-modified", last_modified)?;
    resp.insert_header("cache-control", cache_control)?;
    insert_extra(&mut resp, extra)?;
    session.write_response_header(Box::new(resp), true).await
}

async fn write_error(
    session: &mut Session,
    status: u16,
    msg: &'static str,
    extra: &[(String, String)],
) -> Result<()> {
    let body = Bytes::from_static(msg.as_bytes());
    let mut resp = ResponseHeader::build(status, Some(2 + extra.len()))?;
    resp.insert_header("content-type", "text/plain")?;
    resp.insert_header("content-length", body.len().to_string())?;
    insert_extra(&mut resp, extra)?;
    session.write_response_header(Box::new(resp), false).await?;
    session.write_response_body(Some(body), true).await
}

// ── Utilities ──────────────────────────────────────────────────────────────

fn make_cache_control(options: &StaticOptions) -> String {
    if let Some(age) = options.max_age.as_deref() {
        if let Ok(d) = humantime::parse_duration(age) {
            return format!("public, max-age={}", d.as_secs());
        }
    }
    "no-cache".to_string()
}

fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let s = header.strip_prefix("bytes=")?;
    if let Some(suffix) = s.strip_prefix('-') {
        let n: u64 = suffix.trim().parse().ok()?;
        if n == 0 || n > total {
            return None;
        }
        return Some((total - n, total - 1));
    }
    let (start_s, end_s) = s.split_once('-')?;
    let start: u64 = start_s.trim().parse().ok()?;
    let end: u64 = if end_s.trim().is_empty() {
        total - 1
    } else {
        end_s.trim().parse().ok()?
    };
    if start > end || end >= total {
        return None;
    }
    Some((start, end))
}

/// Attempt to decode a two-byte hex sequence (the digits after `%`) into a
/// raw byte.  Returns `None` if the sequence is invalid or encodes a path
/// separator (`/` or `\`), which must never be decoded to prevent traversal.
fn try_decode_percent_seq(hex: &[u8]) -> Option<u8> {
    let hs = std::str::from_utf8(hex).ok()?;
    let byte = u8::from_str_radix(hs, 16).ok()?;
    // Never decode %2F ('/') or %5C ('\') — path separators allow traversal.
    if byte == b'/' || byte == b'\\' {
        return None;
    }
    Some(byte)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(byte) = try_decode_percent_seq(&bytes[i + 1..i + 3]) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn sanitize_path(path: &str) -> String {
    // Normalize backslashes to forward slashes before splitting so that
    // Windows path separators cannot be used to bypass traversal checks.
    let normalized = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for segment in normalized.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn has_dotfile(rel_path: &str) -> bool {
    rel_path
        .split('/')
        .any(|seg| seg.starts_with('.') && !seg.is_empty())
}

/// Decode and sanitize the request path, optionally stripping a route prefix.
fn decode_rel_path(req_path: &str, strip_prefix: Option<&str>) -> String {
    let decoded = percent_decode(req_path);
    match strip_prefix {
        Some(pfx) => {
            let trimmed = pfx.trim_end_matches('/');
            let after = if trimmed.is_empty() {
                decoded.as_str()
            } else {
                decoded.strip_prefix(trimmed).unwrap_or(&decoded)
            };
            sanitize_path(after)
        }
        None => sanitize_path(&decoded),
    }
}

/// Return `true` when the request is fresh and should receive a 304 response.
///
/// Checks `If-None-Match` first (taking precedence over `If-Modified-Since`
/// per RFC 9110 §13.1).  Per RFC 9110 §13.1.2, `If-None-Match` may contain a
/// comma-separated list of ETags; the server must match against any of them.
fn is_not_modified(hdrs: &http::HeaderMap, etag: &str, mtime: std::time::SystemTime) -> bool {
    if let Some(inm) = hdrs.get("if-none-match").and_then(|v| v.to_str().ok()) {
        // A wildcard matches any ETag; otherwise check each comma-separated value.
        // Per RFC 9110 the list may look like: `"abc123", "def456"`.
        return inm == "*" || inm.split(',').any(|token| token.trim() == etag);
    }
    if let Some(ims) = hdrs.get("if-modified-since").and_then(|v| v.to_str().ok()) {
        if let Ok(ims_time) = httpdate::parse_http_date(ims) {
            return mtime <= ims_time + Duration::from_secs(1);
        }
    }
    false
}
