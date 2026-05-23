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
            Ok(val) => result.push_str(&val),
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

    #[test]
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
}
