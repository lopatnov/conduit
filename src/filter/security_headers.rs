use crate::config::schema::{SecurityHeadersConfig, SecurityHeadersOptions};

/// Build the security response headers for the given config.
///
/// Returns an empty vec when `securityHeaders: false`.
pub fn header_entries(cfg: &SecurityHeadersConfig) -> Vec<(String, String)> {
    match cfg {
        SecurityHeadersConfig::Enabled(false) => vec![],
        SecurityHeadersConfig::Enabled(true) => defaults(),
        SecurityHeadersConfig::Options(opts) => custom(opts),
    }
}

fn defaults() -> Vec<(String, String)> {
    vec![
        ("X-Content-Type-Options".to_owned(), "nosniff".to_owned()),
        ("X-Frame-Options".to_owned(), "SAMEORIGIN".to_owned()),
        (
            "Referrer-Policy".to_owned(),
            "strict-origin-when-cross-origin".to_owned(),
        ),
        ("X-XSS-Protection".to_owned(), "1; mode=block".to_owned()),
    ]
}

fn custom(opts: &SecurityHeadersOptions) -> Vec<(String, String)> {
    let mut h = Vec::new();

    // These two are always included — there is no valid reason to omit them.
    h.push(("X-Content-Type-Options".to_owned(), "nosniff".to_owned()));
    h.push(("X-XSS-Protection".to_owned(), "1; mode=block".to_owned()));

    let frame = opts.x_frame_options.as_deref().unwrap_or("SAMEORIGIN");
    h.push(("X-Frame-Options".to_owned(), frame.to_owned()));

    let referrer = opts
        .referrer_policy
        .as_deref()
        .unwrap_or("strict-origin-when-cross-origin");
    h.push(("Referrer-Policy".to_owned(), referrer.to_owned()));

    if let Some(secs) = opts.hsts_max_age_secs {
        h.push((
            "Strict-Transport-Security".to_owned(),
            format!("max-age={secs}; includeSubDomains"),
        ));
    }

    if let Some(csp) = &opts.csp {
        h.push(("Content-Security-Policy".to_owned(), csp.clone()));
    }

    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_empty() {
        assert!(header_entries(&SecurityHeadersConfig::Enabled(false)).is_empty());
    }

    #[test]
    fn enabled_returns_defaults() {
        let h = header_entries(&SecurityHeadersConfig::Enabled(true));
        assert!(h
            .iter()
            .any(|(k, v)| k == "X-Content-Type-Options" && v == "nosniff"));
        assert!(h
            .iter()
            .any(|(k, v)| k == "X-Frame-Options" && v == "SAMEORIGIN"));
        assert!(h.iter().any(|(k, _)| k == "Referrer-Policy"));
        assert!(h.iter().any(|(k, _)| k == "X-XSS-Protection"));
    }

    #[test]
    fn options_with_hsts_and_csp() {
        let opts = SecurityHeadersOptions {
            hsts_max_age_secs: Some(31_536_000),
            csp: Some("default-src 'self'".to_owned()),
            x_frame_options: Some("DENY".to_owned()),
            referrer_policy: None,
        };
        let h = header_entries(&SecurityHeadersConfig::Options(opts));
        assert!(h
            .iter()
            .any(|(k, v)| k == "Strict-Transport-Security" && v.starts_with("max-age=31536000")));
        assert!(h
            .iter()
            .any(|(k, v)| k == "Content-Security-Policy" && v == "default-src 'self'"));
        assert!(h.iter().any(|(k, v)| k == "X-Frame-Options" && v == "DENY"));
    }
}
