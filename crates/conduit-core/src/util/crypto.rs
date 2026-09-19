use subtle::ConstantTimeEq as _;

/// Constant-time byte-string equality for credential comparisons.
///
/// Using `==` for password/API-key matching leaks timing information: the
/// comparison short-circuits on the first differing byte, letting an attacker
/// determine how many leading bytes of a guessed credential are correct.
///
/// This helper always inspects every byte regardless of where a difference
/// occurs.  Length is checked first (NOT constant-time) — a length mismatch
/// still reveals the correct length, but the set of lengths that need to be
/// tried is already known (API keys are fixed-length by convention).
///
/// Promoted here from the root crate's `src/filter/auth.rs` (issue
/// [#114](https://github.com/lopatnov/conduit/issues/114)/[#134](https://github.com/lopatnov/conduit/issues/134))
/// so both the root crate's always-on Basic Auth / API-key guards
/// (`check_credentials`/`check_api_key`) and `crates/conduit-auth-consumers`'
/// consumer-credential checks share one real implementation instead of each
/// keeping their own copy.
#[inline]
pub fn ct_eq_str(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.as_bytes().ct_eq(b.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_identical_strings() {
        assert!(ct_eq_str("secret", "secret"));
    }

    #[test]
    fn ct_eq_different_strings() {
        assert!(!ct_eq_str("secret", "wrong"));
    }

    #[test]
    fn ct_eq_different_lengths() {
        assert!(!ct_eq_str("short", "longer-value"));
        assert!(!ct_eq_str("longer-value", "short"));
    }

    #[test]
    fn ct_eq_empty_strings() {
        assert!(ct_eq_str("", ""));
    }

    #[test]
    fn ct_eq_empty_vs_nonempty() {
        assert!(!ct_eq_str("", "x"));
        assert!(!ct_eq_str("x", ""));
    }

    #[test]
    fn ct_eq_unicode_strings() {
        // Unicode strings — length in bytes matters, not chars.
        assert!(ct_eq_str("café", "café"));
        assert!(!ct_eq_str("café", "cafe"));
    }
}
