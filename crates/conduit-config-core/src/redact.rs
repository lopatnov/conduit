//! Secret redaction shared by every crate that prints config values (issue #471: these helpers used to be
//! copied per crate).
//!
//! Nothing here names a schema type or a Cargo feature, so it belongs to this crate's "zero schema
//! knowledge" invariant.

use std::borrow::Cow;
use std::fmt;

/// Marker printed in place of a secret value in a manual `Debug` impl —
/// distinguishing "present" (`Some([REDACTED])`) from "absent" (`None`)
/// without ever printing the actual value (issue #354: every secret-bearing
/// config field used to derive plain `Debug`, so a `{:?}`-formatted print or
/// a panic message that happened to include one would leak it verbatim).
#[derive(Clone)]
pub struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

/// Redact any embedded credentials (`user:pass@`) from a Redis URL before
/// logging it — `redis://user:pass@host:port` must never appear in logs
/// verbatim. Security review finding on issue #330's fix: this log line
/// used to be unreachable in practice, since the pre-fix connect path
/// panicked before ever getting here — now that it's genuinely live on
/// every startup/reload, the credential-in-URL case matters for real.
pub fn redact_url(url: &str) -> Cow<'_, str> {
    let Some(scheme_end) = url.find("://") else {
        return Cow::Borrowed(url);
    };
    let authority_start = scheme_end + 3;
    let rest = &url[authority_start..];
    // Bound the search to the authority component only — everything up to
    // the first '/' (or the whole remainder, if there's no path).
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    // The LAST '@' within the authority is the userinfo/host separator, not
    // the first — this codebase's `$VAR` secret-interpolation model has no
    // URL-encoding step, so a raw '@' inside a password is realistic (a
    // second review-round finding on PR #331: `find` here previously leaked
    // a fragment of a password containing its own '@').
    let Some(at) = authority.rfind('@') else {
        return Cow::Borrowed(url);
    };
    Cow::Owned(format!(
        "{}***@{}",
        &url[..authority_start],
        &rest[at + 1..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_debug_prints_a_marker_not_a_value() {
        assert_eq!(format!("{:?}", Some(Redacted)), "Some([REDACTED])");
    }

    #[test]
    fn redact_url_strips_username_and_password() {
        assert_eq!(
            redact_url("redis://alice:s3cret@example.com:6379"),
            "redis://***@example.com:6379"
        );
    }

    #[test]
    fn redact_url_no_credentials_returned_unchanged() {
        let url = "redis://example.com:6379";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn redact_url_handles_rediss_scheme() {
        assert_eq!(
            redact_url("rediss://user:pw@secure.example:6380"),
            "rediss://***@secure.example:6380"
        );
    }

    #[test]
    fn redact_url_does_not_treat_an_at_sign_in_a_path_as_credentials() {
        // No '@' before the first '/' after the scheme -- not userinfo.
        let url = "redis://example.com:6379/db@1";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn redact_url_malformed_url_returned_unchanged() {
        let url = "not-a-url";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn redact_url_password_containing_at_sign_does_not_leak_a_fragment() {
        // Regression for a second review-round finding on PR #331: this
        // codebase's `$VAR` secret-interpolation model has no URL-encoding
        // step, so a raw '@' inside a password is realistic. The first '@'
        // is part of the password, not the userinfo/host separator -- using
        // the LAST '@' in the authority is the only correct split.
        let redacted = redact_url("redis://user:pa@ss@host:6379");
        assert_eq!(redacted, "redis://***@host:6379");
        assert!(
            !redacted.contains("pa") && !redacted.contains("ss"),
            "no fragment of the password must survive redaction: {redacted}"
        );
    }
}
