//! Checking the host name a certificate is ordered for.
//!
//! The ACME flow keeps `<storage>/<domain>.crt.pem` and `<storage>/<domain>.key.pem`, where
//! `<domain>` is the site's `host` (issue #554). A name with a path separator or `..` in it would
//! move those files out of the storage directory, so the name is checked twice: here, when the
//! config is validated, and again where the file names are built (`flow::storage::pair_paths`).
//! Always compiled, like [`crate::AcmeConfig`], so `conduit validate` checks it in every build.

/// Longest name in presentation form: 255 octets on the wire, minus the length octet and the root
/// label (RFC 1035 section 2.3.4).
const MAX_NAME_LEN: usize = 253;

/// Longest single label (RFC 1035 section 2.3.4).
const MAX_LABEL_LEN: usize = 63;

/// `Ok` when `domain` can safely be both an ACME identifier and part of a file name; otherwise
/// the reason, worded to follow "tls.acme needs a plain DNS host name:".
///
/// Accepted: dot-separated labels of ASCII letters, digits and hyphens (no label starting or
/// ending with a hyphen), with an optional leading `*.` label. International names must be given
/// in their punycode (`xn--`) form, which is also what the CA expects. Rejected: path separators,
/// `..`, NUL and any other character a file name could be bent by, empty labels (this includes a
/// leading or trailing dot), and the match-any host `*`.
///
/// A wildcard is accepted because it is a valid ACME identifier; a CA validating over HTTP-01,
/// the only challenge Conduit implements, will normally refuse to issue for it.
pub fn validate_domain(domain: &str) -> Result<(), &'static str> {
    if domain.is_empty() {
        return Err("the host is empty");
    }
    if domain == "*" {
        return Err("the host '*' matches any host; set the concrete name the certificate is for");
    }
    if domain.len() > MAX_NAME_LEN {
        return Err("the host is longer than 253 characters");
    }
    let name = domain.strip_prefix("*.").unwrap_or(domain);
    for label in name.split('.') {
        if label.is_empty() {
            return Err("the host has an empty label (a leading, trailing or doubled dot)");
        }
        if label.len() > MAX_LABEL_LEN {
            return Err("the host has a label longer than 63 characters");
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(
                "the host may contain only ASCII letters, digits, hyphens and dots \
                 (write international names in punycode, 'xn--…')",
            );
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("the host has a label that starts or ends with a hyphen");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for ok in [
            "example.com",
            "a.example.com",
            "localhost",
            "xn--bcher-kva.example",
            "EXAMPLE.com",
            "a-b.example.com",
            "*.example.com",
            "127.0.0.1",
            "1.example.com",
        ] {
            assert_eq!(validate_domain(ok), Ok(()), "{ok} should be accepted");
        }
    }

    /// The names that would move the certificate or key out of the storage directory, or name
    /// something other than a file in it, are all rejected (issue #554).
    #[test]
    fn rejects_path_traversal_and_separators() {
        for bad in [
            "..",
            ".",
            "../x",
            "../../etc/x",
            "a/b",
            "/etc/passwd",
            "a\\b",
            "..\\x",
            "a/../b",
            "x/..",
            ".hidden.example.com",
            "a\0b",
            "a\0",
            "example.com/",
            "C:\\x",
            "a:b",
            "~root",
        ] {
            assert!(validate_domain(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn rejects_malformed_names() {
        for bad in [
            "",
            "*",
            "*.",
            "*.*.example.com",
            "a*.example.com",
            "example.*",
            "example.com.",
            ".example.com",
            "a..b",
            "-a.example.com",
            "a-.example.com",
            "a b.example.com",
            "a\nb.example.com",
            "a\tb",
            "bücher.example",
            "exa%2Fmple.com",
            "example.com:8443",
            "user@example.com",
        ] {
            assert!(validate_domain(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn enforces_length_limits() {
        let label63 = "a".repeat(63);
        assert_eq!(validate_domain(&format!("{label63}.example.com")), Ok(()));
        let label64 = "a".repeat(64);
        assert!(validate_domain(&format!("{label64}.example.com")).is_err());

        // 63 + 1 + 63 + 1 + 63 + 1 + 61 = 253 characters is the longest accepted name.
        let longest = [label63.as_str(), &label63, &label63, &"a".repeat(61)].join(".");
        assert_eq!(longest.len(), 253);
        assert_eq!(validate_domain(&longest), Ok(()));
        assert!(validate_domain(&format!("{longest}a")).is_err());
    }
}
