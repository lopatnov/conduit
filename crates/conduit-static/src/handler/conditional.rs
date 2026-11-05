//! Conditional requests and ranges: the response validators, `If-None-Match` / `If-Modified-Since` /
//! `If-Range`, `Range` parsing and `Cache-Control`.

use std::time::Duration;

use pingora_core::Result;
use pingora_http::ResponseHeader;

use crate::config::StaticOptions;

/// The response validators and the content-negotiation marker of one static response.
///
/// `etag` / `last_modified` are `None` when the site switched them off (`static.etag: false`,
/// `static.lastModified: false`): the header is then neither sent nor consulted in `If-None-Match`,
/// `If-Modified-Since` and `If-Range`. `vary_encoding` is true when the same URL can be answered with
/// different `Content-Encoding`s (on-the-fly compression or pre-compressed siblings are enabled), so
/// *every* representation, the uncompressed one and the `304`/`206` included, must say
/// `Vary: Accept-Encoding` or a shared cache can hand a compressed body to a client that cannot
/// decode it (RFC 9110 §12.5.5, RFC 9111 §4.1).
pub(super) struct Validators<'a> {
    pub(super) etag: Option<&'a str>,
    pub(super) last_modified: Option<&'a str>,
    pub(super) vary_encoding: bool,
}

impl Validators<'_> {
    /// Add the validator headers this response carries: `ETag`, `Last-Modified` and `Vary: Accept-Encoding`.
    pub(super) fn apply(&self, resp: &mut ResponseHeader) -> Result<()> {
        if let Some(etag) = self.etag {
            resp.insert_header("etag", etag)?;
        }
        if let Some(last_modified) = self.last_modified {
            resp.insert_header("last-modified", last_modified)?;
        }
        if self.vary_encoding {
            resp.insert_header("vary", "accept-encoding")?;
        }
        Ok(())
    }

    /// Header slots `apply` may fill, for `ResponseHeader::build`'s capacity hint.
    pub(super) const SLOTS: usize = 3;
}

/// Whether a `Range` request may be answered with `206` given its `If-Range` header (RFC 9110 §13.1.5).
///
/// No `If-Range` → yes. An entity-tag must match our (strong) ETag exactly — a weak tag never
/// matches — and a date must equal `Last-Modified` exactly; anything else, or an unparsable value,
/// means "the resource may have changed": ignore the `Range` and send the full file.
pub(super) fn if_range_allows_range(
    headers: &http::HeaderMap,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> bool {
    let Some(value) = headers.get("if-range") else {
        return true;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let value = value.trim();
    // A validator the site switched off was never sent, so it cannot match.
    if value.starts_with('"') {
        etag == Some(value)
    } else {
        !value.starts_with("W/") && last_modified == Some(value)
    }
}

/// `Cache-Control` for the site's `maxAge`: `public, max-age=N`; `no-cache` when it is not set or not a duration.
pub(super) fn make_cache_control(options: &StaticOptions) -> String {
    if let Some(age) = options.max_age.as_deref() {
        if let Ok(d) = humantime::parse_duration(age) {
            return format!("public, max-age={}", d.as_secs());
        }
    }
    "no-cache".to_string()
}

/// Parse one `bytes=` range against the file's `total` size into inclusive `(start, end)` offsets (the
/// open-ended and suffix forms included). `None` for a malformed range or one that starts past the end:
/// the caller answers `416`.
pub(super) fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
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

/// Return `true` when the request is fresh and should receive a 304 response.
///
/// Checks `If-None-Match` first (taking precedence over `If-Modified-Since`
/// per RFC 9110 §13.1).  Per RFC 9110 §13.1.2, `If-None-Match` may contain a
/// comma-separated list of ETags; the server must match against any of them.
///
/// `etag` is `None` and `use_last_modified` false when the site switched that validator off; the
/// matching precondition header is then ignored (RFC 9110 §13.1: a recipient ignores a condition it
/// cannot evaluate), and the request is served in full.
pub(super) fn is_not_modified(
    hdrs: &http::HeaderMap,
    etag: Option<&str>,
    use_last_modified: bool,
    mtime: std::time::SystemTime,
) -> bool {
    if let Some(inm) = hdrs.get("if-none-match") {
        // RFC 9110 §13.1.3: `If-Modified-Since` is ignored whenever `If-None-Match` is present, so
        // with the ETag switched off the request is served in full instead of falling through to
        // the date condition.
        let (Some(etag), Ok(inm)) = (etag, inm.to_str()) else {
            return false;
        };
        // A wildcard matches any ETag; otherwise check each comma-separated value.
        // Per RFC 9110 the list may look like: `"abc123", "def456"`.
        return inm == "*" || inm.split(',').any(|token| token.trim() == etag);
    }
    if !use_last_modified {
        return false;
    }
    if let Some(ims) = hdrs.get("if-modified-since").and_then(|v| v.to_str().ok()) {
        if let Ok(ims_time) = httpdate::parse_http_date(ims) {
            return mtime <= ims_time + Duration::from_secs(1);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #402 (RFC 9110 §13.1.5): `If-Range` decides whether `Range` may be honoured.
    #[test]
    fn if_range_only_allows_a_range_when_the_validator_is_current() {
        let etag = "\"65f-1a\"";
        let lm = "Sat, 10 Oct 2026 01:00:00 GMT";
        let with = |v: &str| {
            let mut h = http::HeaderMap::new();
            h.insert("if-range", http::HeaderValue::from_str(v).unwrap());
            h
        };
        assert!(if_range_allows_range(
            &http::HeaderMap::new(),
            Some(etag),
            Some(lm)
        ));
        assert!(if_range_allows_range(&with(etag), Some(etag), Some(lm)));
        assert!(if_range_allows_range(&with(lm), Some(etag), Some(lm)));
        assert!(!if_range_allows_range(
            &with("\"other\""),
            Some(etag),
            Some(lm)
        ));
        assert!(!if_range_allows_range(
            &with("W/\"65f-1a\""),
            Some(etag),
            Some(lm)
        ));
        assert!(!if_range_allows_range(
            &with("Sat, 10 Oct 2026 00:59:59 GMT"),
            Some(etag),
            Some(lm)
        ));
        // A validator the site switched off was never sent, so a matching-looking value is stale.
        assert!(!if_range_allows_range(&with(etag), None, Some(lm)));
        assert!(!if_range_allows_range(&with(lm), Some(etag), None));
        assert!(!if_range_allows_range(
            &with("garbage"),
            Some(etag),
            Some(lm)
        ));
    }

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
        assert!(!is_not_modified(
            &http::HeaderMap::new(),
            Some(ETAG),
            true,
            MTIME
        ));
    }

    #[test]
    fn if_none_match_exact_returns_true() {
        let hdrs = headers_with("if-none-match", ETAG);
        assert!(is_not_modified(&hdrs, Some(ETAG), true, MTIME));
    }

    #[test]
    fn if_none_match_without_etag_ignores_if_modified_since() {
        // RFC 9110 §13.1.3: with the ETag off, a present If-None-Match still suppresses the date check.
        let mut hdrs = headers_with("if-none-match", "*");
        let ims = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(3600));
        hdrs.insert("if-modified-since", ims.parse().unwrap());
        assert!(!is_not_modified(
            &hdrs,
            None,
            true,
            std::time::SystemTime::now()
        ));
    }

    #[test]
    fn if_none_match_wildcard_returns_true() {
        let hdrs = headers_with("if-none-match", "*");
        assert!(is_not_modified(&hdrs, Some(ETAG), true, MTIME));
    }

    #[test]
    fn if_none_match_different_etag_returns_false() {
        let hdrs = headers_with("if-none-match", r#""different""#);
        assert!(!is_not_modified(&hdrs, Some(ETAG), true, MTIME));
    }

    #[test]
    fn if_none_match_list_with_match_returns_true() {
        let hdrs = headers_with("if-none-match", r#""other1", "abc123", "other2""#);
        assert!(is_not_modified(&hdrs, Some(ETAG), true, MTIME));
    }

    #[test]
    fn if_modified_since_same_time_returns_true() {
        // File was not modified since the given time.
        let ims = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(3600));
        let hdrs = headers_with("if-modified-since", &ims);
        let mtime = SystemTime::now();
        assert!(is_not_modified(&hdrs, Some(ETAG), true, mtime));
    }

    #[test]
    fn if_modified_since_older_than_mtime_returns_false() {
        // File was modified AFTER the If-Modified-Since header.
        let ims = httpdate::fmt_http_date(UNIX_EPOCH);
        let hdrs = headers_with("if-modified-since", &ims);
        let mtime = SystemTime::now(); // very recent
        assert!(!is_not_modified(&hdrs, Some(ETAG), true, mtime));
    }
}
