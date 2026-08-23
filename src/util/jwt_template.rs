//! `{{ jwt.<claim> }}` template expansion for `requestTransform.setHeaders` values.
//!
//! Lives outside `filter::jwt` (which requires `--features jwt`) because the
//! header-transform code path that calls this is always compiled: a config
//! can reference `{{ jwt.<claim> }}` syntax regardless of whether the `jwt`
//! feature is enabled. Claims are only ever populated by
//! `#[cfg(feature = "jwt")]`-gated code elsewhere, so without that feature
//! `claims` is always `None` here and every template resolves to `""` —
//! matching this crate's existing behavior before this module was split out.

/// Expand `{{ jwt.<claim> }}` templates in a string.
///
/// Replaces all occurrences of `{{ jwt.CLAIM }}` with the corresponding value
/// from the JWT payload. Unknown claims are replaced with an empty string.
/// Non-string claim values are JSON-serialized (e.g. numbers, arrays).
pub(crate) fn expand_jwt_templates(
    template: &str,
    claims: &Option<std::collections::HashMap<String, serde_json::Value>>,
) -> String {
    static JWT_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = JWT_RE.get_or_init(|| {
        regex::Regex::new(r"\{\{\s*jwt\.(\w+)\s*\}\}").expect("jwt template regex")
    });

    re.replace_all(template, |caps: &regex::Captures<'_>| {
        let claim_name = &caps[1];
        match claims {
            Some(map) => match map.get(claim_name) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            },
            None => String::new(),
        }
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_jwt_templates_sub() {
        let mut claims = std::collections::HashMap::new();
        claims.insert("sub".to_string(), serde_json::json!("alice"));
        claims.insert("role".to_string(), serde_json::json!("admin"));

        assert_eq!(
            expand_jwt_templates("{{ jwt.sub }}", &Some(claims.clone())),
            "alice"
        );
        assert_eq!(
            expand_jwt_templates(
                "user={{ jwt.sub }},role={{ jwt.role }}",
                &Some(claims.clone())
            ),
            "user=alice,role=admin"
        );
        // Unknown claim → empty string.
        assert_eq!(expand_jwt_templates("{{ jwt.unknown }}", &Some(claims)), "");
        // No claims → empty string.
        assert_eq!(expand_jwt_templates("{{ jwt.sub }}", &None), "");
    }

    #[test]
    fn expand_jwt_templates_no_template_unchanged() {
        let claims: std::collections::HashMap<String, serde_json::Value> = Default::default();
        let result = expand_jwt_templates("plain-value", &Some(claims));
        assert_eq!(result, "plain-value");
    }

    #[test]
    fn expand_jwt_templates_null_claims_returns_empty() {
        let result = expand_jwt_templates("{{ jwt.sub }}", &None);
        assert_eq!(result, "");
    }

    #[test]
    fn expand_jwt_templates_non_string_claim_is_json_serialized() {
        let mut claims = std::collections::HashMap::new();
        claims.insert("age".to_string(), serde_json::json!(42));
        claims.insert("roles".to_string(), serde_json::json!(["admin", "user"]));

        assert_eq!(
            expand_jwt_templates("{{ jwt.age }}", &Some(claims.clone())),
            "42"
        );
        assert_eq!(
            expand_jwt_templates("{{ jwt.roles }}", &Some(claims)),
            r#"["admin","user"]"#
        );
    }
}
