//! Static-file serving handler.
//!
//! Moved from `src/handler/static_files.rs` (issue #114/#139) — see this
//! crate's `src/lib.rs` doc comment.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

#[cfg(feature = "compression")]
use async_compression::tokio::bufread::{BrotliEncoder, DeflateEncoder, GzipEncoder};
#[cfg(feature = "compression")]
use async_compression::Level;
use async_trait::async_trait;
use bytes::Bytes;
#[cfg(feature = "compression")]
use conduit_compression::logic::CompressOptions;
use conduit_core::handler::LocalHandlerImpl;
use conduit_core::util::encoding::AcceptEncoding;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
#[cfg(feature = "compression")]
use tokio::io::AsyncRead;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::config::{FallbackConfig, StaticOptions};
use crate::mime;

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
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        let found = handle_static(
            session,
            &self.roots,
            &self.options,
            self.strip_prefix.as_deref(),
            &self.extra_headers,
            #[cfg(feature = "compression")]
            self.compress_opts.as_ref(),
            #[cfg(not(feature = "compression"))]
            None,
            &self.accept_enc,
        )
        .await?;

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
pub async fn handle_static(
    session: &mut Session,
    roots: &[PathBuf],
    options: &Arc<StaticOptions>,
    strip_prefix: Option<&str>,
    extra: &[(String, String)],
    #[cfg(feature = "compression")] compress_opts: Option<&CompressOptions>,
    #[cfg(not(feature = "compression"))] _compress_opts: Option<()>,
    accept_enc: &AcceptEncoding,
) -> Result<bool> {
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

    let Some(file_path) = find_file(roots, &rel, options).await else {
        return Ok(false);
    };

    // Re-check with symlink_metadata to guard against a TOCTOU race:
    // between find_file() rejecting symlinks and this point, another process
    // could have replaced the file with a symlink.  Reject if it's now
    // a symlink or has disappeared.
    let meta = match tokio::fs::symlink_metadata(&file_path).await {
        Ok(m) if !m.file_type().is_symlink() => m,
        _ => return Ok(false),
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
        if let Some((pre_path, encoding, pre_size)) =
            resolve_pre_compressed(&file_path, accept_enc).await
        {
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

/// Open `path` for reading without following symlinks (nginx pattern).
///
/// On Linux/macOS: uses `O_NOFOLLOW` which makes the open syscall fail
/// atomically when the final path component is a symbolic link.  This
/// eliminates the TOCTOU window between stat_no_symlink() and open().
///
/// On Windows: falls back to a regular open (symlinks there require
/// elevated privileges to create, so the risk is lower).
async fn open_no_follow(path: &Path) -> std::io::Result<tokio::fs::File> {
    #[cfg(unix)]
    {
        // O_NOFOLLOW is 0x20000 on Linux and 0x100 on macOS — use the
        // platform constant via std's OpenOptionsExt trait.
        let std_file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        Ok(tokio::fs::File::from_std(std_file))
    }
    #[cfg(not(unix))]
    {
        tokio::fs::File::open(path).await
    }
}

/// Map a file I/O error to a Pingora-level internal error.
fn io_err(e: std::io::Error) -> Box<pingora_core::Error> {
    pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string())
}

/// Check `path` metadata WITHOUT following symlinks.
///
/// Returns `None` when the path does not exist or is a symbolic link.
/// Symlinks are rejected to prevent directory traversal: a symlink inside
/// the static root could point anywhere on the filesystem (e.g.
/// `/var/www/html/secret` → `/etc/passwd`), bypassing `sanitize_path`'s
/// `..`-traversal protection.
async fn stat_no_symlink(path: &std::path::Path) -> Option<std::fs::Metadata> {
    let meta = tokio::fs::symlink_metadata(path).await.ok()?;
    if meta.is_symlink() {
        return None; // Reject — symlink could escape the static root.
    }
    Some(meta)
}

async fn find_file(roots: &[PathBuf], rel: &str, options: &StaticOptions) -> Option<PathBuf> {
    for root in roots {
        let candidate = root.join(rel);
        match stat_no_symlink(&candidate).await {
            Some(m) if m.is_file() => return Some(candidate),
            Some(m) if m.is_dir() => {
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
        if stat_no_symlink(&p)
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
    let candidates: &[(&'static str, &'static str)] = &[("br", ".br"), ("gzip", ".gz")];

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
        if stat_no_symlink(&pre_path)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return Some((pre_path, enc));
        }
    }
    None
}

/// Re-stat an already-`find_pre_compressed`-confirmed candidate, returning
/// its current size.
///
/// Split out from [`resolve_pre_compressed`] specifically so this half —
/// the actual TOCTOU guard — is independently testable: calling
/// `resolve_pre_compressed` again after removing the file would re-run
/// `find_pre_compressed` too and short-circuit before ever reaching this
/// re-stat, so a test built that way would prove nothing about this
/// function's own failure handling.
///
/// On a failed re-stat (the sibling was removed or replaced since
/// `find_pre_compressed` confirmed it), returns `None` so the caller falls
/// through to serving the uncompressed original instead of defaulting to a
/// `pre_size` of `0`, which would send a 200 with `content-length: 0` for
/// an asset that actually exists uncompressed.
async fn confirm_pre_compressed(
    pre_path: PathBuf,
    encoding: &'static str,
) -> Option<(PathBuf, &'static str, u64)> {
    let pre_size = stat_no_symlink(&pre_path).await?.len();
    Some((pre_path, encoding, pre_size))
}

/// Resolve a pre-compressed sibling to serve, including its current size.
///
/// Guards the same TOCTOU window as the main file check — see
/// [`confirm_pre_compressed`]'s doc comment for why the re-stat is a
/// separate function rather than inlined here.
async fn resolve_pre_compressed(
    path: &Path,
    accept_enc: &AcceptEncoding,
) -> Option<(PathBuf, &'static str, u64)> {
    let (pre_path, encoding) = find_pre_compressed(path, accept_enc).await?;
    confirm_pre_compressed(pre_path, encoding).await
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
///
/// `compress` (in `handle_static`) is only ever `Some` when the `compression`
/// feature is compiled in (see its `#[cfg(not(feature = "compression"))]`
/// fallback, which forces it to `None`) — so the `#[cfg(not(feature =
/// "compression"))]` variant below is unreachable in practice; it exists only
/// so this function type-checks (and drops `async-compression` from the
/// dependency tree) in a `compression`-less build.
#[cfg(feature = "compression")]
async fn stream_file_compressed(
    session: &mut Session,
    path: &Path,
    offset: u64,
    length: u64,
    encoding: &str,
    level: u8,
) -> Result<()> {
    let mut file = open_no_follow(path).await.map_err(io_err)?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io_err)?;
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

/// See the `#[cfg(feature = "compression")]` overload above — unreachable
/// stub for a `compression`-less build (`compress` is always `None` in that
/// build, so `serve_full` never calls this).
#[cfg(not(feature = "compression"))]
async fn stream_file_compressed(
    session: &mut Session,
    path: &Path,
    offset: u64,
    length: u64,
    _encoding: &str,
    _level: u8,
) -> Result<()> {
    stream_file(session, path, offset, length).await
}

/// Drain an `AsyncRead` encoder in 64 KiB chunks, signalling `done=true` on
/// the last write.
///
/// We use a "one-chunk-ahead" pattern: we always buffer the chunk we just read
/// and send it on the *next* iteration, so we know whether there is more data
/// before we call `write_response_body`.  This lets us set `done=true` on the
/// final chunk without reading an extra zero-length chunk first.
#[cfg(feature = "compression")]
async fn stream_encoded<R: AsyncRead + Unpin>(session: &mut Session, mut reader: R) -> Result<()> {
    const CHUNK: usize = 64 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut pending: Option<Bytes> = None;

    loop {
        let n = reader.read(&mut buf).await.map_err(io_err)?;

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
    let mut file = open_no_follow(path).await.map_err(io_err)?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io_err)?;
    }
    let mut remaining = length;
    let mut buf = vec![0u8; CHUNK];
    while remaining > 0 {
        let to_read = (remaining as usize).min(CHUNK);
        let n = file.read(&mut buf[..to_read]).await.map_err(io_err)?;
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
    if total == 0 {
        // No satisfiable range exists for an empty file — bail out before
        // any `total - 1` subtraction, which would underflow (panics with
        // overflow checks enabled, wraps and returns None otherwise).
        return None;
    }
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
/// raw byte.  Returns `None` if the sequence is invalid or encodes a
/// dangerous byte.
fn try_decode_percent_seq(hex: &[u8]) -> Option<u8> {
    let hs = std::str::from_utf8(hex).ok()?;
    let byte = u8::from_str_radix(hs, 16).ok()?;
    // Never decode dangerous bytes:
    // • %2F '/' and %5C '\' — path separators enable directory traversal.
    // • %00 NUL — null bytes truncate paths at the OS syscall boundary on
    //   POSIX systems (e.g. `file.php%00.txt` → opens `file.php`).
    match byte {
        b'/' | b'\\' | b'\0' => None,
        b => Some(b),
    }
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
            // A colon-bearing segment enables two distinct Windows-specific
            // escapes if let through: a drive prefix like `C:` makes
            // `PathBuf::join` treat the rest as an absolute path (replacing
            // `root` entirely, since a prefix+root is "absolute" per
            // `Path::join`'s own documented behavior — verified: `find_file`'s
            // `root.join(rel)` would resolve straight to `C:/Windows/win.ini`,
            // not under `root`), and `name.txt:stream` targets an NTFS
            // alternate data stream instead of the plain file. Neither shape
            // is meaningful outside Windows — `:` is an ordinary, legal
            // filename character on Linux/macOS (this repo's actual runtime
            // targets), so gate the drop to Windows builds only rather than
            // silently mismatching a legitimately colon-named file on every
            // platform (Gitar + security-engineer review on PR #348). Drop
            // the segment rather than rejecting the whole request — matches
            // how "."/".."/empty segments are already silently filtered out
            // just above.
            s if cfg!(windows) && s.contains(':') => {}
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

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Windows symlink creation requires elevated privileges in CI, so this test only
    // runs on unix — `std::os::unix::fs::symlink` isn't available on Windows anyway.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_in_static_root_is_not_served() {
        // stat_no_symlink must return None for symlinks — serving them would
        // allow directory traversal (symlink → /etc/passwd, etc.).
        let dir = tempfile::tempdir().unwrap();
        let real_file = dir.path().join("real.txt");
        std::fs::write(&real_file, "contents").unwrap();

        // Create a symlink pointing to the real file.
        let link_path = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real_file, &link_path).unwrap();

        // stat_no_symlink must reject the symlink.
        assert!(
            stat_no_symlink(&link_path).await.is_none(),
            "symlink must not be served by static file handler"
        );
        // Regular file is fine.
        assert!(
            stat_no_symlink(&real_file).await.is_some(),
            "regular file must be served"
        );
    }

    // ── resolve_pre_compressed ────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_pre_compressed_returns_size_when_sibling_exists() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("app.js");
        std::fs::write(&base, "irrelevant").unwrap();
        std::fs::write(dir.path().join("app.js.gz"), b"compressed-bytes").unwrap();

        let accept_enc = AcceptEncoding {
            gzip: true,
            ..Default::default()
        };
        let (pre_path, encoding, pre_size) = resolve_pre_compressed(&base, &accept_enc)
            .await
            .expect("gz sibling exists and is confirmed by both stats");
        assert_eq!(pre_path, dir.path().join("app.js.gz"));
        assert_eq!(encoding, "gzip");
        assert_eq!(pre_size, "compressed-bytes".len() as u64);
    }

    #[tokio::test]
    async fn confirm_pre_compressed_returns_size_when_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let gz_path = dir.path().join("app.js.gz");
        std::fs::write(&gz_path, b"compressed-bytes").unwrap();

        let (returned_path, encoding, pre_size) = confirm_pre_compressed(gz_path.clone(), "gzip")
            .await
            .expect("file exists, re-stat must succeed");
        assert_eq!(returned_path, gz_path);
        assert_eq!(encoding, "gzip");
        assert_eq!(pre_size, "compressed-bytes".len() as u64);
    }

    #[tokio::test]
    async fn confirm_pre_compressed_falls_through_when_file_removed() {
        // Directly exercises the TOCTOU guard itself (not resolve_pre_compressed,
        // which would re-run find_pre_compressed internally and short-circuit
        // before ever reaching this re-stat if the file were already gone —
        // confirm_pre_compressed exists precisely so this failure path is
        // reachable in a deterministic test). Before the fix, the caller
        // defaulted `pre_size` to 0 and still served a 200 with an empty body
        // for a file confirmed present moments earlier; the fix must return
        // `None` here so the caller falls through to the uncompressed original.
        let dir = tempfile::tempdir().unwrap();
        let gz_path = dir.path().join("app.js.gz");
        std::fs::write(&gz_path, b"compressed-bytes").unwrap();

        // Confirm the happy path works before removing the file.
        assert!(
            confirm_pre_compressed(gz_path.clone(), "gzip")
                .await
                .is_some(),
            "sanity: re-stat must succeed while the file still exists"
        );

        std::fs::remove_file(&gz_path).unwrap();

        assert!(
            confirm_pre_compressed(gz_path, "gzip").await.is_none(),
            "must fall through, not default pre_size to 0, when the re-stat fails"
        );
    }

    #[test]
    fn percent_decode_null_byte_is_rejected() {
        // %00 must NOT be decoded — it would truncate the path at the OS level.
        let decoded = percent_decode("file%00.txt");
        assert!(
            !decoded.contains('\0'),
            "null byte must not appear in decoded path: {decoded:?}"
        );
        // The literal `%00` should remain undecoded, not become NUL.
        assert_eq!(
            decoded, "file%00.txt",
            "undecoded %00 must be preserved as-is"
        );
    }

    #[test]
    fn percent_decode_slash_not_decoded() {
        let decoded = percent_decode("%2Fetc%2Fpasswd");
        assert_eq!(decoded, "%2Fetc%2Fpasswd", "%2F must not be decoded");
    }

    #[test]
    fn percent_decode_normal_chars_decoded() {
        let decoded = percent_decode("hello%20world");
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn sanitize_path_removes_dotdot() {
        assert_eq!(sanitize_path("../etc/passwd"), "etc/passwd");
        assert_eq!(sanitize_path("a/../../b"), "b");
    }

    // ── sanitize_path extra cases ─────────────────────────────────────────────

    #[test]
    fn sanitize_path_removes_backslash_traversal() {
        // Backslashes are normalized to forward slashes first.
        assert_eq!(sanitize_path("a\\..\\b"), "b");
    }

    #[test]
    fn sanitize_path_collapses_empty_segments() {
        assert_eq!(sanitize_path("a//b"), "a/b");
    }

    #[test]
    fn sanitize_path_removes_single_dot_segments() {
        assert_eq!(sanitize_path("a/./b"), "a/b");
    }

    #[test]
    fn sanitize_path_empty_input() {
        assert_eq!(sanitize_path(""), "");
    }

    #[test]
    fn sanitize_path_root_slash() {
        assert_eq!(sanitize_path("/"), "");
    }

    #[test]
    fn sanitize_path_normal_path() {
        assert_eq!(sanitize_path("images/logo.png"), "images/logo.png");
    }

    #[test]
    #[cfg(windows)]
    fn sanitize_path_drops_windows_drive_prefix() {
        // `root.join("C:/Windows/win.ini")` on Windows treats the drive
        // prefix as an absolute path and replaces `root` entirely — the
        // colon-bearing segment must never survive sanitization. Windows-only:
        // `:` is an ordinary filename character on Linux/macOS, where this
        // escape doesn't exist (see sanitize_path_keeps_colon_on_non_windows).
        assert_eq!(sanitize_path("C:/Windows/win.ini"), "Windows/win.ini");
        assert_eq!(sanitize_path("/C:/Windows/win.ini"), "Windows/win.ini");
    }

    #[test]
    #[cfg(windows)]
    fn sanitize_path_drops_ntfs_alternate_data_stream_suffix() {
        // `name.txt:hidden` targets an NTFS alternate data stream on Windows
        // rather than the plain file — reject the whole segment, not just
        // the drive-prefix shape.
        assert_eq!(sanitize_path("secret.txt:hidden"), "");
    }

    #[test]
    #[cfg(not(windows))]
    fn sanitize_path_keeps_colon_on_non_windows() {
        // `:` is a legal filename character on Linux/macOS (this repo's
        // actual runtime targets) and neither Windows escape (drive-prefix
        // absolute-path replacement, NTFS alternate data streams) exists
        // there — a legitimately colon-named file must stay reachable
        // rather than being silently dropped/mismatched (Gitar +
        // security-engineer review on PR #348).
        assert_eq!(sanitize_path("secret.txt:hidden"), "secret.txt:hidden");
    }

    // ── has_dotfile ───────────────────────────────────────────────────────────

    #[test]
    fn has_dotfile_hidden_file() {
        assert!(has_dotfile(".hidden"), "leading dot must be a dotfile");
    }

    #[test]
    fn has_dotfile_hidden_dir() {
        assert!(has_dotfile(".git/config"), "hidden dir must be a dotfile");
    }

    #[test]
    fn has_dotfile_normal_file() {
        assert!(!has_dotfile("images/logo.png"));
    }

    #[test]
    fn has_dotfile_double_dot_is_dotfile() {
        // ".." starts with '.' so has_dotfile returns true — this is intentional
        // since dotfile blocking should also reject ".." segments (sanitize_path
        // handles the actual traversal prevention separately).
        assert!(has_dotfile(".."));
    }

    #[test]
    fn has_dotfile_empty_string() {
        assert!(!has_dotfile(""));
    }

    // ── parse_range ───────────────────────────────────────────────────────────

    #[test]
    fn parse_range_simple() {
        // bytes=100-200 with total=1000 → (100, 200)
        assert_eq!(parse_range("bytes=100-200", 1000), Some((100, 200)));
    }

    #[test]
    fn parse_range_open_end() {
        // bytes=100- → (100, total-1)
        assert_eq!(parse_range("bytes=100-", 1000), Some((100, 999)));
    }

    #[test]
    fn parse_range_suffix() {
        // bytes=-200 → (800, 999)
        assert_eq!(parse_range("bytes=-200", 1000), Some((800, 999)));
    }

    #[test]
    fn parse_range_invalid_no_bytes_prefix() {
        assert!(parse_range("0-100", 1000).is_none());
    }

    #[test]
    fn parse_range_start_beyond_end() {
        // bytes=500-100 (start > end) → invalid
        assert!(parse_range("bytes=500-100", 1000).is_none());
    }

    #[test]
    fn parse_range_end_beyond_total() {
        // bytes=0-9999 with total=1000 → invalid (end >= total)
        assert!(parse_range("bytes=0-9999", 1000).is_none());
    }

    #[test]
    fn parse_range_suffix_zero_invalid() {
        // bytes=-0 → zero suffix length is invalid
        assert!(parse_range("bytes=-0", 1000).is_none());
    }

    #[test]
    fn parse_range_empty_file_no_panic() {
        // total=0 must not reach the `total - 1` subtraction (underflow
        // panics with overflow checks enabled) — no range is satisfiable
        // for an empty file regardless of header shape.
        assert!(parse_range("bytes=0-", 0).is_none());
        assert!(parse_range("bytes=0-0", 0).is_none());
        assert!(parse_range("bytes=-1", 0).is_none());
    }

    // ── make_cache_control ────────────────────────────────────────────────────

    #[test]
    fn make_cache_control_no_max_age_returns_no_cache() {
        let opts = StaticOptions::default();
        assert_eq!(make_cache_control(&opts), "no-cache");
    }

    #[test]
    fn make_cache_control_with_max_age_returns_public() {
        let opts = StaticOptions {
            max_age: Some("1h".to_owned()),
            ..Default::default()
        };
        let cc = make_cache_control(&opts);
        assert!(
            cc.starts_with("public, max-age="),
            "expected public max-age: {cc}"
        );
        assert!(cc.contains("3600"), "1h = 3600s: {cc}");
    }

    #[test]
    fn make_cache_control_invalid_duration_returns_no_cache() {
        let opts = StaticOptions {
            max_age: Some("not-a-duration".to_owned()),
            ..Default::default()
        };
        assert_eq!(make_cache_control(&opts), "no-cache");
    }

    // ── try_decode_percent_seq ────────────────────────────────────────────────

    #[test]
    fn try_decode_percent_seq_normal_char() {
        // %41 = 'A'
        assert_eq!(try_decode_percent_seq(b"41"), Some(b'A'));
    }

    #[test]
    fn try_decode_percent_seq_space() {
        // %20 = space
        assert_eq!(try_decode_percent_seq(b"20"), Some(b' '));
    }

    #[test]
    fn try_decode_percent_seq_slash_rejected() {
        // %2F = '/' — must NOT be decoded
        assert!(try_decode_percent_seq(b"2F").is_none());
        assert!(try_decode_percent_seq(b"2f").is_none());
    }

    #[test]
    fn try_decode_percent_seq_backslash_rejected() {
        // %5C = '\' — must NOT be decoded
        assert!(try_decode_percent_seq(b"5C").is_none());
    }

    #[test]
    fn try_decode_percent_seq_null_rejected() {
        // %00 = NUL — must NOT be decoded
        assert!(try_decode_percent_seq(b"00").is_none());
    }

    #[test]
    fn try_decode_percent_seq_invalid_hex_returns_none() {
        assert!(try_decode_percent_seq(b"ZZ").is_none());
        assert!(try_decode_percent_seq(b"GG").is_none());
    }

    // ── decode_rel_path ───────────────────────────────────────────────────────

    #[test]
    fn decode_rel_path_no_prefix() {
        assert_eq!(decode_rel_path("/images/logo.png", None), "images/logo.png");
    }

    #[test]
    fn decode_rel_path_strips_prefix() {
        assert_eq!(
            decode_rel_path("/api/images/logo.png", Some("/api")),
            "images/logo.png"
        );
    }

    #[test]
    fn decode_rel_path_percent_decoded() {
        assert_eq!(
            decode_rel_path("/files/hello%20world.txt", None),
            "files/hello world.txt"
        );
    }

    #[test]
    fn decode_rel_path_traversal_prevented() {
        // Even after percent-decoding, .. should be removed by sanitize_path.
        assert_eq!(decode_rel_path("/../etc/passwd", None), "etc/passwd");
    }
}

#[cfg(test)]
mod tests_extra {
    use super::*;
    use pingora_http::ResponseHeader;

    #[test]
    fn insert_extra_adds_headers() {
        let mut resp = ResponseHeader::build(200, None).unwrap();
        let extra = vec![
            ("x-custom".to_owned(), "value1".to_owned()),
            ("x-another".to_owned(), "value2".to_owned()),
        ];
        insert_extra(&mut resp, &extra).unwrap();
        assert_eq!(resp.headers.get("x-custom").unwrap(), "value1");
        assert_eq!(resp.headers.get("x-another").unwrap(), "value2");
    }

    #[test]
    fn insert_extra_empty_is_noop() {
        let mut resp = ResponseHeader::build(200, None).unwrap();
        insert_extra(&mut resp, &[]).unwrap();
        assert_eq!(resp.headers.len(), 0);
    }
}

#[cfg(test)]
mod tests_is_not_modified {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn headers_with(key: &str, value: &str) -> http::HeaderMap {
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert(
            http::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            http::header::HeaderValue::from_str(value).unwrap(),
        );
        hdrs
    }

    const ETAG: &str = r#""abc123""#;
    const MTIME: std::time::SystemTime = UNIX_EPOCH; // epoch, always in past

    #[test]
    fn no_conditional_headers_returns_false() {
        assert!(!is_not_modified(&http::HeaderMap::new(), ETAG, MTIME));
    }

    #[test]
    fn if_none_match_exact_returns_true() {
        let hdrs = headers_with("if-none-match", ETAG);
        assert!(is_not_modified(&hdrs, ETAG, MTIME));
    }

    #[test]
    fn if_none_match_wildcard_returns_true() {
        let hdrs = headers_with("if-none-match", "*");
        assert!(is_not_modified(&hdrs, ETAG, MTIME));
    }

    #[test]
    fn if_none_match_different_etag_returns_false() {
        let hdrs = headers_with("if-none-match", r#""different""#);
        assert!(!is_not_modified(&hdrs, ETAG, MTIME));
    }

    #[test]
    fn if_none_match_list_with_match_returns_true() {
        let hdrs = headers_with("if-none-match", r#""other1", "abc123", "other2""#);
        assert!(is_not_modified(&hdrs, ETAG, MTIME));
    }

    #[test]
    fn if_modified_since_same_time_returns_true() {
        // File was not modified since the given time.
        let ims = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(3600));
        let hdrs = headers_with("if-modified-since", &ims);
        let mtime = SystemTime::now();
        assert!(is_not_modified(&hdrs, ETAG, mtime));
    }

    #[test]
    fn if_modified_since_older_than_mtime_returns_false() {
        // File was modified AFTER the If-Modified-Since header.
        let ims = httpdate::fmt_http_date(UNIX_EPOCH);
        let hdrs = headers_with("if-modified-since", &ims);
        let mtime = SystemTime::now(); // very recent
        assert!(!is_not_modified(&hdrs, ETAG, mtime));
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
