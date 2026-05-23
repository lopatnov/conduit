use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use bytes::Bytes;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncSeekExt;

use crate::config::schema::StaticOptions;
use crate::util::mime;

pub async fn handle_static(
    session: &mut Session,
    roots: &[PathBuf],
    options: &Arc<StaticOptions>,
    strip_prefix: Option<&str>,
) -> Result<()> {
    let method = session.req_header().method.clone();
    let req_path = session.req_header().uri.path().to_owned();

    let decoded = percent_decode(&req_path);
    let rel = match strip_prefix {
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
    };

    let dot_policy = options.dot_files.as_deref().unwrap_or("ignore");
    if has_dotfile(&rel) {
        return match dot_policy {
            "deny" => write_error(session, 403, "Forbidden").await,
            _ => write_error(session, 404, "Not Found").await,
        };
    }

    let Some(file_path) = find_file(roots, &rel, options).await else {
        return write_error(session, 404, "Not Found").await;
    };

    let meta = match tokio::fs::metadata(&file_path).await {
        Ok(m) => m,
        Err(_) => return write_error(session, 404, "Not Found").await,
    };

    let file_size = meta.len();
    let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
    let mtime_secs = mtime.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let etag = format!("\"{mtime_secs:x}-{file_size:x}\"");
    let last_modified = httpdate::fmt_http_date(mtime);
    let cache_control = make_cache_control(options);
    let content_type = mime::content_type(&file_path).to_string();

    let hdrs = session.req_header().headers.clone();

    // If-None-Match
    if let Some(inm) = hdrs.get("if-none-match").and_then(|v| v.to_str().ok()) {
        if inm == etag || inm == "*" {
            return write_not_modified(session, &etag, &last_modified, &cache_control).await;
        }
    }

    // If-Modified-Since (only when no If-None-Match)
    if hdrs.get("if-none-match").is_none() {
        if let Some(ims) = hdrs.get("if-modified-since").and_then(|v| v.to_str().ok()) {
            if let Ok(ims_time) = httpdate::parse_http_date(ims) {
                // Allow 1-second rounding
                if mtime <= ims_time + Duration::from_secs(1) {
                    return write_not_modified(session, &etag, &last_modified, &cache_control)
                        .await;
                }
            }
        }
    }

    let is_head = method.as_str() == "HEAD";

    // Range request
    if let Some(range_hdr) = hdrs.get("range").and_then(|v| v.to_str().ok()) {
        return serve_range(
            session,
            &file_path,
            file_size,
            range_hdr.to_owned(),
            &content_type,
            &etag,
            &last_modified,
            &cache_control,
            is_head,
        )
        .await;
    }

    serve_full(
        session,
        &file_path,
        file_size,
        &content_type,
        &etag,
        &last_modified,
        &cache_control,
        is_head,
    )
    .await
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
    // For empty rel (root path), try index directly on each root
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
        if tokio::fs::metadata(&p).await.map(|m| m.is_file()).unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

// ── Response helpers ───────────────────────────────────────────────────────

async fn serve_full(
    session: &mut Session,
    path: &Path,
    size: u64,
    content_type: &str,
    etag: &str,
    last_modified: &str,
    cache_control: &str,
    is_head: bool,
) -> Result<()> {
    let mut resp = ResponseHeader::build(200, Some(6))?;
    resp.insert_header("content-type", content_type)?;
    resp.insert_header("content-length", size.to_string())?;
    resp.insert_header("etag", etag)?;
    resp.insert_header("last-modified", last_modified)?;
    resp.insert_header("cache-control", cache_control)?;
    resp.insert_header("accept-ranges", "bytes")?;

    if is_head {
        session.write_response_header(Box::new(resp), true).await?;
        return Ok(());
    }

    session.write_response_header(Box::new(resp), false).await?;

    let body = tokio::fs::read(path)
        .await
        .map_err(|e| pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string()))?;
    session
        .write_response_body(Some(Bytes::from(body)), true)
        .await
}

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
) -> Result<()> {
    let Some((start, end)) = parse_range(&range_hdr, total) else {
        let mut resp = ResponseHeader::build(416, Some(2))?;
        resp.insert_header("content-range", format!("bytes */{total}"))?;
        resp.insert_header("content-length", "0")?;
        session.write_response_header(Box::new(resp), true).await?;
        return Ok(());
    };

    let length = end - start + 1;
    let mut resp = ResponseHeader::build(206, Some(7))?;
    resp.insert_header("content-type", content_type)?;
    resp.insert_header("content-length", length.to_string())?;
    resp.insert_header("content-range", format!("bytes {start}-{end}/{total}"))?;
    resp.insert_header("etag", etag)?;
    resp.insert_header("last-modified", last_modified)?;
    resp.insert_header("cache-control", cache_control)?;
    resp.insert_header("accept-ranges", "bytes")?;

    if is_head {
        session.write_response_header(Box::new(resp), true).await?;
        return Ok(());
    }

    session.write_response_header(Box::new(resp), false).await?;

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string()))?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string()))?;

    let mut buf = vec![0u8; length as usize];
    file.read_exact(&mut buf)
        .await
        .map_err(|e| pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string()))?;
    session
        .write_response_body(Some(Bytes::from(buf)), true)
        .await
}

async fn write_not_modified(
    session: &mut Session,
    etag: &str,
    last_modified: &str,
    cache_control: &str,
) -> Result<()> {
    let mut resp = ResponseHeader::build(304, Some(3))?;
    resp.insert_header("etag", etag)?;
    resp.insert_header("last-modified", last_modified)?;
    resp.insert_header("cache-control", cache_control)?;
    session.write_response_header(Box::new(resp), true).await
}

async fn write_error(session: &mut Session, status: u16, msg: &'static str) -> Result<()> {
    let body = Bytes::from_static(msg.as_bytes());
    let mut resp = ResponseHeader::build(status, Some(2))?;
    resp.insert_header("content-type", "text/plain")?;
    resp.insert_header("content-length", body.len().to_string())?;
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

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &bytes[i + 1..i + 3];
            if let Ok(hs) = std::str::from_utf8(hex) {
                if let Ok(byte) = u8::from_str_radix(hs, 16) {
                    // Never decode %2F ('/') — that would allow path traversal
                    if byte != b'/' {
                        out.push(byte);
                        i += 3;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn sanitize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
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
    rel_path.split('/').any(|seg| seg.starts_with('.') && !seg.is_empty())
}
