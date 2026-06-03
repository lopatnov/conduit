use std::env;

/// Replace `$VAR_NAME` patterns in config text with environment variable values.
/// If the variable is not set, the original `$VAR_NAME` is kept unchanged.
/// Interpolation is done at the raw text level before JSON parsing.
pub fn interpolate(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '$' {
            result.push(c);
            continue;
        }

        // Must start with letter or underscore (env var naming convention)
        match chars.peek() {
            Some(&first) if first.is_ascii_alphabetic() || first == '_' => {}
            _ => {
                result.push('$');
                continue;
            }
        }

        let mut var_name = String::new();
        while let Some(&next) = chars.peek() {
            if next.is_ascii_alphanumeric() || next == '_' {
                var_name.push(next);
                chars.next();
            } else {
                break;
            }
        }

        match env::var(&var_name) {
            Ok(val) => {
                // JSON-escape the value so that special characters (quotes,
                // backslashes, control chars) cannot break the JSON structure
                // or inject unexpected keys/values.
                // serde_json::to_string produces `"escaped"` — strip the quotes
                // since the $VAR placeholder is already inside a JSON string.
                let escaped = serde_json::to_string(&val)
                    .map(|s| s[1..s.len() - 1].to_owned())
                    .unwrap_or(val);
                result.push_str(&escaped);
            }
            Err(_) => {
                result.push('$');
                result.push_str(&var_name);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn replaces_known_var() {
        std::env::set_var("CONDUIT_TEST_TOKEN", "secret");
        let out = interpolate(r#"{"token": "$CONDUIT_TEST_TOKEN"}"#);
        assert_eq!(out, r#"{"token": "secret"}"#);
    }

    #[test]
    fn keeps_unknown_var() {
        let out = interpolate(r#"{"token": "$CONDUIT_NONEXISTENT_XYZ_987"}"#);
        assert_eq!(out, r#"{"token": "$CONDUIT_NONEXISTENT_XYZ_987"}"#);
    }

    #[test]
    fn ignores_bare_dollar() {
        let out = interpolate("price: $42");
        assert_eq!(out, "price: $42");
    }

    #[test]
    fn ignores_double_dollar() {
        let out = interpolate("$$VAR");
        assert_eq!(out, "$$VAR");
    }

    #[test]
    #[serial]
    fn escapes_quotes_in_value() {
        std::env::set_var("CONDUIT_TEST_QUOTE", r#"say "hello""#);
        let out = interpolate(r#"{"msg": "$CONDUIT_TEST_QUOTE"}"#);
        // The double-quotes inside the value must be escaped so the JSON stays valid.
        assert_eq!(out, r#"{"msg": "say \"hello\""}"#);
    }

    #[test]
    #[serial]
    fn escapes_backslash_in_value() {
        std::env::set_var("CONDUIT_TEST_BACKSLASH", r"C:\path\to\file");
        let out = interpolate(r#"{"path": "$CONDUIT_TEST_BACKSLASH"}"#);
        assert_eq!(out, r#"{"path": "C:\\path\\to\\file"}"#);
    }

    #[test]
    fn var_at_end_of_string() {
        let out = interpolate("value=$CONDUIT_NONEXISTENT_END");
        assert_eq!(out, "value=$CONDUIT_NONEXISTENT_END");
    }

    #[test]
    fn var_followed_by_special_char() {
        let out = interpolate("$CONDUIT_NONEXISTENT_SPEC.");
        assert_eq!(out, "$CONDUIT_NONEXISTENT_SPEC.");
    }

    #[test]
    fn no_substitution_when_empty() {
        let out = interpolate("");
        assert_eq!(out, "");
    }

    #[test]
    fn dollar_at_end() {
        let out = interpolate("trailing$");
        assert_eq!(out, "trailing$");
    }

    #[test]
    #[serial]
    fn multiple_vars_in_one_string() {
        std::env::set_var("CONDUIT_TEST_A", "hello");
        std::env::set_var("CONDUIT_TEST_B", "world");
        let out = interpolate("$CONDUIT_TEST_A $CONDUIT_TEST_B");
        assert_eq!(out, "hello world");
    }
}
