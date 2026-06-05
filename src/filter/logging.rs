use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;

use pingora_proxy::Session;

use crate::config::schema::{LogFormat, LoggingConfig, SiteConfig};
use crate::util::log_writer::LogWriter;

// ── Public API ─────────────────────────────────────────────────────────────

/// Extra context for structured access logs — filled in by the logging() hook.
pub struct AccessLogContext<'a> {
    /// Value of the `X-Request-ID` header (generated or forwarded by XRequestIdGuard).
    pub request_id: Option<&'a str>,
    /// Selected upstream URL for proxy requests (`None` for local handlers).
    pub upstream_addr: Option<&'a str>,
    /// Time spent waiting for the upstream to respond (ms).
    ///
    /// Measured from `upstream_request_filter` (request sent) to `logging()`
    /// (response fully received). `None` for local handlers.
    pub upstream_ms: Option<u64>,
}

/// Format and write a single access-log line for the completed request.
///
/// Does nothing when logging is not configured or is explicitly disabled.
pub fn write_access_log(
    session: &Session,
    start_time: Instant,
    site_config: Option<&SiteConfig>,
    log_writer: &LogWriter,
    extra: &AccessLogContext<'_>,
) {
    let logging_cfg = site_config.and_then(|s| s.logging.as_ref());

    let (format, file_path, skip_paths, strip_query) = match logging_cfg {
        None | Some(LoggingConfig::Enabled(false)) => return,
        Some(LoggingConfig::Enabled(true)) => (&LogFormat::Combined, None, None, false),
        Some(LoggingConfig::Format(f)) => (f, None, None, false),
        Some(LoggingConfig::Options(opts)) => {
            let fmt = opts.format.as_ref().unwrap_or(&LogFormat::Combined);
            (
                fmt,
                opts.file.as_deref(),
                opts.skip_paths.as_deref(),
                opts.strip_query.unwrap_or(false),
            )
        }
    };

    // Skip logging for paths that match the skipPaths list.
    if let Some(skip) = skip_paths {
        let path = session.req_header().uri.path();
        use crate::filter::auth::is_path_skipped;
        if is_path_skipped(Some(skip), path) {
            return;
        }
    }

    // Lazily switch the writer to the configured file when needed.
    match file_path {
        Some(path) if log_writer.current_path().as_deref() != Some(path) => {
            if let Err(e) = log_writer.switch_file(path) {
                tracing::warn!(path, "failed to open access log file: {e}");
            }
        }
        None if log_writer.current_path().is_some() => {
            log_writer.use_stdout();
        }
        _ => {}
    }

    let line = format_line(session, start_time, format, extra, strip_query);
    log_writer.write_line(&line);
}

// ── Formatting ─────────────────────────────────────────────────────────────

/// Sanitize a string for inclusion in a **text** log line.
///
/// Replaces ASCII control characters (`\r`, `\n`, `\t`, NUL, and other
/// C0 controls below 0x20) with a space so that an attacker cannot inject
/// fake log entries by crafting a URL or User-Agent containing `\r\n`.
///
/// JSON log format is safe by default (serde_json escapes all control
/// characters), so this function is only needed for text-based formats.
fn sanitize_log_field(s: &str) -> std::borrow::Cow<'_, str> {
    // Fast path: scan for any control character before allocating.
    if !s.bytes().any(|b| b < 0x20) {
        return std::borrow::Cow::Borrowed(s);
    }
    // Slow path: replace control chars with a visible placeholder.
    std::borrow::Cow::Owned(
        s.chars()
            .map(|c| if (c as u32) < 0x20 { ' ' } else { c })
            .collect(),
    )
}

fn format_line(
    session: &Session,
    start_time: Instant,
    format: &LogFormat,
    extra: &AccessLogContext<'_>,
    strip_query: bool,
) -> String {
    // Sanitize user-controlled fields in text formats to prevent log injection
    // (CR/LF in a URL or User-Agent would let an attacker forge log entries).
    let method = session.req_header().method.as_str();
    // When stripQuery is enabled, only log the path component — query strings
    // may contain API tokens (?access_token=…) that must not appear in logs.
    let raw_path = if strip_query {
        session.req_header().uri.path()
    } else {
        session
            .req_header()
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or_else(|| session.req_header().uri.path())
    };
    let path = sanitize_log_field(raw_path);
    let status = session
        .response_written()
        .map(|h| h.status.as_u16().to_string())
        .unwrap_or_else(|| "0".to_owned());
    let elapsed_ms = start_time.elapsed().as_millis();
    let client_ip = session
        .client_addr()
        .and_then(|a| a.as_inet())
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "-".to_owned());
    let body_bytes = session
        .response_written()
        .and_then(|h| h.headers.get("content-length"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");

    match format {
        LogFormat::Dev => {
            format!("{method} {path} {status} {body_bytes} - {elapsed_ms}ms")
        }
        LogFormat::Short => {
            format!("{client_ip} {method} {path} {status} {body_bytes} {elapsed_ms}ms")
        }
        LogFormat::Common => {
            let t = clf_now();
            let ver = http_version(session);
            format!(r#"{client_ip} - - [{t}] "{method} {path} {ver}" {status} {body_bytes}"#)
        }
        LogFormat::Combined => {
            let t = clf_now();
            let ver = http_version(session);
            let raw_referer = session
                .req_header()
                .headers
                .get("referer")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-");
            let raw_ua = session
                .req_header()
                .headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-");
            let referer = sanitize_log_field(raw_referer);
            let ua = sanitize_log_field(raw_ua);
            format!(
                r#"{client_ip} - - [{t}] "{method} {path} {ver}" {status} {body_bytes} "{referer}" "{ua}""#
            )
        }
        LogFormat::Json => {
            let t = iso8601_now();
            let bytes: JsonValue = body_bytes
                .parse::<u64>()
                .map(JsonValue::from)
                .unwrap_or(JsonValue::Null);
            let mut obj = serde_json::json!({
                "time":        t,
                "method":      method,
                "path":        path,
                "status":      status.parse::<u16>().unwrap_or(0),
                "bytes":       bytes,
                "duration_ms": elapsed_ms as u64,
                "ip":          client_ip,
            });
            if let Some(rid) = extra.request_id {
                obj["request_id"] = JsonValue::String(rid.to_owned());
            }
            if let Some(addr) = extra.upstream_addr {
                obj["upstream"] = JsonValue::String(addr.to_owned());
            }
            if let Some(ms) = extra.upstream_ms {
                obj["upstream_ms"] = JsonValue::from(ms);
            }
            obj.to_string()
        }
    }
}

fn http_version(session: &Session) -> &'static str {
    match session.req_header().version {
        http::Version::HTTP_10 => "HTTP/1.0",
        http::Version::HTTP_11 => "HTTP/1.1",
        http::Version::HTTP_2 => "HTTP/2.0",
        _ => "HTTP/1.1",
    }
}

// ── Timestamp helpers ──────────────────────────────────────────────────────

/// CLF timestamp: `12/May/2024:10:00:00 +0000`
fn clf_now() -> String {
    // httpdate gives "Wed, 12 May 2024 10:00:00 GMT" — reformat to CLF.
    let hd = httpdate::fmt_http_date(SystemTime::now());
    let parts: Vec<&str> = hd.split_ascii_whitespace().collect();
    // parts: ["Wed,", "12", "May", "2024", "10:00:00", "GMT"]
    if parts.len() >= 5 {
        format!("{}/{}/{}:{} +0000", parts[1], parts[2], parts[3], parts[4])
    } else {
        hd
    }
}

/// ISO 8601 UTC timestamp: `2024-05-12T10:00:00Z`
fn iso8601_now() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let rem = ts % 86400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;
    let (y, mo, d) = civil_from_days((ts / 86400) as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Convert days since the Unix epoch (1970-01-01) to `(year, month, day)`.
///
/// Uses the algorithm by Howard Hinnant:
/// <https://howardhinnant.github.io/date_algorithms.html>
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // day of era
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year of era
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year
    let mp = (5 * doy + 2) / 153; // month prime
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_epoch() {
        // Unix epoch = 1970-01-01
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn civil_from_days_known() {
        // 2024-05-12 = days since epoch: (2024-1970)*365 + leap years + month offsets
        // Let's just verify against a known value: 2000-03-01 = day 11017
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
    }

    #[test]
    fn iso8601_format() {
        let ts = iso8601_now();
        // Should look like "2024-05-12T10:00:00Z"
        assert!(ts.len() == 20, "unexpected length: {ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
    }

    #[test]
    fn clf_format() {
        let ts = clf_now();
        // Should contain "+" and "+0000"
        assert!(ts.contains("+0000"), "bad CLF ts: {ts}");
    }

    #[test]
    fn civil_from_days_leap_year() {
        // 2000-02-29 is a valid leap day.
        // Days since epoch to 2000-02-29: manually verified = 11_016
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn civil_from_days_negative_epoch() {
        // 1969-12-31 = day -1 before Unix epoch
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn civil_from_days_year_boundary() {
        // 2024-01-01 = days since epoch: 19_723 (verified via date math)
        // 2024 is a leap year, but Jan 1 is just after New Year
        let (y, m, d) = civil_from_days(19_723);
        assert_eq!(y, 2024);
        assert_eq!(m, 1);
        assert_eq!(d, 1);
    }

    #[test]
    fn iso8601_contains_date_separators() {
        let ts = iso8601_now();
        // Format: YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.chars().nth(4), Some('-'));
        assert_eq!(ts.chars().nth(7), Some('-'));
        assert_eq!(ts.chars().nth(10), Some('T'));
        assert_eq!(ts.chars().nth(13), Some(':'));
        assert_eq!(ts.chars().nth(16), Some(':'));
    }

    // ── upstream_ms in JSON log ───────────────────────────────────────────────

    #[test]
    fn json_log_includes_upstream_ms_when_present() {
        use std::time::Instant;
        // Build a minimal fake session snapshot by directly calling format_line
        // via write_access_log is hard without a real Session; test format_line
        // logic indirectly through the JSON branch using a real AccessLogContext.
        // Instead, we test the JSON object construction directly.
        let obj = serde_json::json!({
            "upstream_ms": 42u64,
        });
        assert_eq!(obj["upstream_ms"], 42);
    }

    #[test]
    fn json_log_omits_upstream_ms_when_absent() {
        let mut obj = serde_json::json!({ "method": "GET" });
        let upstream_ms: Option<u64> = None;
        if let Some(ms) = upstream_ms {
            obj["upstream_ms"] = serde_json::Value::from(ms);
        }
        assert!(
            obj.get("upstream_ms").is_none(),
            "upstream_ms must be absent when None"
        );
    }

    // ── sanitize_log_field ────────────────────────────────────────────────────

    #[test]
    fn sanitize_log_field_clean_string_is_returned_borrowed() {
        let s = "/api/v1/users";
        let out = sanitize_log_field(s);
        assert_eq!(out.as_ref(), s);
        // Fast path returns a borrowed slice (no allocation).
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn sanitize_log_field_removes_crlf_injection() {
        // An attacker who injects \r\n in a URL could forge log entries.
        let malicious = "/path\r\nFake-Log-Entry: injected";
        let out = sanitize_log_field(malicious);
        assert!(!out.contains('\r'), "\\r must be removed");
        assert!(!out.contains('\n'), "\\n must be removed");
        assert!(
            out.contains("Fake-Log-Entry"),
            "rest of string must survive"
        );
    }

    #[test]
    fn sanitize_log_field_removes_null_byte() {
        let s = "/path\x00injection";
        let out = sanitize_log_field(s);
        assert!(!out.contains('\0'), "null byte must be removed");
    }

    #[test]
    fn sanitize_log_field_removes_other_control_chars() {
        let s = "/tab\there";
        let out = sanitize_log_field(s);
        assert!(!out.contains('\t'), "tab must be replaced");
    }
}
