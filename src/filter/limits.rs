use pingora_proxy::Session;

use crate::config::schema::LimitsConfig;

pub enum CheckResult {
    Ok,
    BodyTooLarge,
    HeaderTooLarge,
}

/// Check declared Content-Length against maxBodyBytes and header size against maxHeaderBytes.
/// Timeout enforcement requires OS-level socket options and is deferred.
pub fn check(config: &LimitsConfig, session: &Session) -> CheckResult {
    let req = session.req_header();

    if let Some(max_header) = config.max_header_bytes {
        if header_size(session) > max_header {
            return CheckResult::HeaderTooLarge;
        }
    }

    if let Some(max_body) = config.max_body_bytes {
        if let Some(cl) = req.headers.get("content-length") {
            if let Ok(s) = cl.to_str() {
                if let Ok(len) = s.parse::<u64>() {
                    if len > max_body {
                        return CheckResult::BodyTooLarge;
                    }
                }
            }
        }
    }

    CheckResult::Ok
}

fn header_size(session: &Session) -> u64 {
    let req = session.req_header();
    // Request line approximation: METHOD SP path SP HTTP/1.1 CRLF
    let request_line = req.method.as_str().len() + 1 + req.uri.to_string().len() + 11;
    let fields: usize = req
        .headers
        .iter()
        .map(|(k, v)| k.as_str().len() + 2 + v.len() + 2) // "name: value\r\n"
        .sum();
    (request_line + fields + 2) as u64 // trailing CRLF
}
