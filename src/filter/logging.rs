use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;

use pingora_proxy::Session;

use crate::config::schema::{LogFormat, LoggingConfig, SiteConfig};
use crate::util::log_writer::LogWriter;

// ── Public API ─────────────────────────────────────────────────────────────

/// Format and write a single access-log line for the completed request.
///
/// Does nothing when logging is not configured or is explicitly disabled.
pub fn write_access_log(
    session: &Session,
    start_time: Instant,
    site_config: Option<&SiteConfig>,
    log_writer: &LogWriter,
) {
    let logging_cfg = site_config.and_then(|s| s.logging.as_ref());

    let (format, file_path) = match logging_cfg {
        None | Some(LoggingConfig::Enabled(false)) => return,
        Some(LoggingConfig::Enabled(true)) => (&LogFormat::Combined, None),
        Some(LoggingConfig::Format(f)) => (f, None),
        Some(LoggingConfig::Options(opts)) => {
            let fmt = opts.format.as_ref().unwrap_or(&LogFormat::Combined);
            (fmt, opts.file.as_deref())
        }
    };

    // Lazily switch the writer to the configured file when needed.
    // The switch is idempotent — LogWriter compares the path and skips re-opens.
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

    let line = format_line(session, start_time, format);
    log_writer.write_line(&line);
}

// ── Formatting ─────────────────────────────────────────────────────────────

fn format_line(session: &Session, start_time: Instant, format: &LogFormat) -> String {
    let method = session.req_header().method.as_str();
    let path = session
        .req_header()
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| session.req_header().uri.path());
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
            let referer = session
                .req_header()
                .headers
                .get("referer")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-");
            let ua = session
                .req_header()
                .headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-");
            format!(
                r#"{client_ip} - - [{t}] "{method} {path} {ver}" {status} {body_bytes} "{referer}" "{ua}""#
            )
        }
        LogFormat::Json => {
            let t = iso8601_now();
            // Parse body_bytes: "-" (no Content-Length) → null; a valid
            // number → JSON integer. Use serde_json for all string fields so
            // that paths containing `"` or `\` don't produce invalid JSON.
            let bytes: JsonValue = body_bytes
                .parse::<u64>()
                .map(JsonValue::from)
                .unwrap_or(JsonValue::Null);
            serde_json::json!({
                "time":        t,
                "method":      method,
                "path":        path,
                "status":      status.parse::<u16>().unwrap_or(0),
                "bytes":       bytes,
                "duration_ms": elapsed_ms as u64,
                "ip":          client_ip,
            })
            .to_string()
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
}
