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
    if config
        .max_header_bytes
        .is_some_and(|max| header_size(session) > max)
    {
        return CheckResult::HeaderTooLarge;
    }

    if config
        .max_body_bytes
        .zip(declared_content_length(session))
        .is_some_and(|(max, len)| len > max)
    {
        return CheckResult::BodyTooLarge;
    }

    CheckResult::Ok
}

/// Parse the Content-Length request header into a byte count, if present and valid.
fn declared_content_length(session: &Session) -> Option<u64> {
    session
        .req_header()
        .headers
        .get("content-length")?
        .to_str()
        .ok()?
        .parse()
        .ok()
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
