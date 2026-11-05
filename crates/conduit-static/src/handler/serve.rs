//! Writing the responses: full, range, pre-compressed, `304` and the small error bodies.

use bytes::Bytes;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;

use super::conditional::{parse_range, Validators};
use super::resolve::PreCompressed;
use super::stream::{stream_file, stream_file_compressed};

/// Union of two `Vary` values, case-insensitive, keeping the first spelling of each token.
fn merge_vary(existing: &str, extra: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for token in existing.split(',').chain(extra.split(',')) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token == "*" {
            return "*".to_owned();
        }
        if !out.iter().any(|t| t.eq_ignore_ascii_case(token)) {
            out.push(token);
        }
    }
    out.join(", ")
}

fn insert_extra(resp: &mut ResponseHeader, extra: &[(String, String)]) -> Result<()> {
    for (name, value) in extra {
        // A configured `Vary` must not drop the `Vary: Accept-Encoding` the validators set: merge.
        if name.eq_ignore_ascii_case("vary") {
            if let Some(existing) = resp.headers.get("vary").and_then(|v| v.to_str().ok()) {
                let merged = merge_vary(existing, value);
                resp.insert_header("vary", merged)?;
                continue;
            }
        }
        resp.insert_header(name.clone(), value.clone())?;
    }
    Ok(())
}

/// What the file responses share, apart from the file itself: the parts of the request and of the
/// configuration (the content type, the validators, `Cache-Control`, `HEAD`, the extra headers).
pub(super) struct RespCtx<'a> {
    pub(super) content_type: &'a str,
    pub(super) validators: &'a Validators<'a>,
    pub(super) cache_control: &'a str,
    pub(super) is_head: bool,
    pub(super) extra: &'a [(String, String)],
}

/// `200` with the whole file. `file` is already open (issue #618): nothing here can fail to open it
/// after the headers are out.
pub(super) async fn serve_full(
    session: &mut Session,
    ctx: &RespCtx<'_>,
    file: tokio::fs::File,
    size: u64,
    compress: Option<(&'static str, u8)>,
) -> Result<()> {
    let extra = ctx.extra;
    if let Some((encoding, level)) = compress {
        // Compressed response — no Content-Length (chunked transfer encoding).
        let header_count = 4 + Validators::SLOTS + extra.len(); // no content-length
        let mut resp = ResponseHeader::build(200, Some(header_count))?;
        resp.insert_header("content-type", ctx.content_type)?;
        ctx.validators.apply(&mut resp)?;
        resp.insert_header("cache-control", ctx.cache_control)?;
        resp.insert_header("accept-ranges", "bytes")?;
        resp.insert_header("content-encoding", encoding)?;
        insert_extra(&mut resp, extra)?;

        if ctx.is_head {
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(());
        }

        session.write_response_header(Box::new(resp), false).await?;
        stream_file_compressed(session, file, 0, size, encoding, level).await
    } else {
        let mut resp = ResponseHeader::build(200, Some(3 + Validators::SLOTS + extra.len()))?;
        resp.insert_header("content-type", ctx.content_type)?;
        resp.insert_header("content-length", size.to_string())?;
        ctx.validators.apply(&mut resp)?;
        resp.insert_header("cache-control", ctx.cache_control)?;
        resp.insert_header("accept-ranges", "bytes")?;
        insert_extra(&mut resp, extra)?;

        if ctx.is_head {
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(());
        }

        session.write_response_header(Box::new(resp), false).await?;
        stream_file(session, file, 0, size).await
    }
}

/// Serve a pre-compressed file directly — no on-the-fly encoding needed.
///
/// Sends the original resource's `Content-Type`, `ETag`, `Last-Modified`, and
/// `Cache-Control`, while adding `Content-Encoding` and `Vary: accept-encoding`
/// to inform caches that this representation is encoding-specific. The sibling was opened (and sized)
/// before the call, so its `Content-Length` is the length of the file this handle reads.
pub(super) async fn serve_pre_compressed(
    session: &mut Session,
    ctx: &RespCtx<'_>,
    pre: PreCompressed,
) -> Result<()> {
    let extra = ctx.extra;
    let PreCompressed { opened, encoding } = pre;
    let mut resp = ResponseHeader::build(200, Some(5 + Validators::SLOTS + extra.len()))?;
    resp.insert_header("content-type", ctx.content_type)?;
    resp.insert_header("content-length", opened.size.to_string())?;
    resp.insert_header("content-encoding", encoding)?;
    ctx.validators.apply(&mut resp)?;
    resp.insert_header("cache-control", ctx.cache_control)?;
    resp.insert_header("accept-ranges", "none")?; // ranges unsupported on pre-compressed
    insert_extra(&mut resp, extra)?;

    if ctx.is_head {
        session.write_response_header(Box::new(resp), true).await?;
        return Ok(());
    }

    session.write_response_header(Box::new(resp), false).await?;
    stream_file(session, opened.file, 0, opened.size).await
}

pub(super) async fn serve_range(
    session: &mut Session,
    ctx: &RespCtx<'_>,
    file: tokio::fs::File,
    total: u64,
    range_hdr: &str,
) -> Result<()> {
    let extra = ctx.extra;
    let Some((start, end)) = parse_range(range_hdr, total) else {
        let mut resp = ResponseHeader::build(416, Some(2 + extra.len()))?;
        resp.insert_header("content-range", format!("bytes */{total}"))?;
        resp.insert_header("content-length", "0")?;
        insert_extra(&mut resp, extra)?;
        session.write_response_header(Box::new(resp), true).await?;
        return Ok(());
    };

    let length = end - start + 1;
    let mut resp = ResponseHeader::build(206, Some(5 + Validators::SLOTS + extra.len()))?;
    resp.insert_header("content-type", ctx.content_type)?;
    resp.insert_header("content-length", length.to_string())?;
    resp.insert_header("content-range", format!("bytes {start}-{end}/{total}"))?;
    ctx.validators.apply(&mut resp)?;
    resp.insert_header("cache-control", ctx.cache_control)?;
    resp.insert_header("accept-ranges", "bytes")?;
    insert_extra(&mut resp, extra)?;

    if ctx.is_head {
        session.write_response_header(Box::new(resp), true).await?;
        return Ok(());
    }

    session.write_response_header(Box::new(resp), false).await?;
    stream_file(session, file, start, length).await
}

pub(super) async fn write_not_modified(
    session: &mut Session,
    validators: &Validators<'_>,
    cache_control: &str,
    extra: &[(String, String)],
) -> Result<()> {
    let mut resp = ResponseHeader::build(304, Some(1 + Validators::SLOTS + extra.len()))?;
    validators.apply(&mut resp)?;
    resp.insert_header("cache-control", cache_control)?;
    insert_extra(&mut resp, extra)?;
    session.write_response_header(Box::new(resp), true).await
}

pub(super) async fn write_error(
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
    fn insert_extra_vary_merges_with_accept_encoding() {
        let mut resp = ResponseHeader::build(200, None).unwrap();
        resp.insert_header("vary", "accept-encoding").unwrap();
        let extra = vec![("Vary".to_owned(), "Origin, Accept-Encoding".to_owned())];
        insert_extra(&mut resp, &extra).unwrap();
        assert_eq!(
            resp.headers.get("vary").unwrap().to_str().unwrap(),
            "accept-encoding, Origin"
        );
    }

    #[test]
    fn merge_vary_star_wins() {
        assert_eq!(merge_vary("accept-encoding", "*"), "*");
    }

    #[test]
    fn insert_extra_empty_is_noop() {
        let mut resp = ResponseHeader::build(200, None).unwrap();
        insert_extra(&mut resp, &[]).unwrap();
        assert_eq!(resp.headers.len(), 0);
    }
}
