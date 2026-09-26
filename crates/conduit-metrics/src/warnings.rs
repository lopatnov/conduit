//! Advisory warnings for `metrics`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::MetricsConfig;

/// Warn when a `metrics` endpoint has no auth token configured. Not feature-gated.
pub fn token_warning(i: usize, config: &MetricsConfig) -> Option<String> {
    if config.token.is_none() {
        Some(format!(
            "sites[{i}].metrics is configured without a token — the \
             /__metrics__ endpoint is publicly accessible. \
             Set metrics.token to require Bearer authentication in production."
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_warning_is_pinned_and_only_without_a_token() {
        assert_eq!(
            token_warning(3, &MetricsConfig::default()),
            Some(
                "sites[3].metrics is configured without a token — the /__metrics__ endpoint \
                 is publicly accessible. Set metrics.token to require Bearer authentication \
                 in production."
                    .to_owned()
            )
        );
        let with_token = MetricsConfig {
            token: Some("t".to_owned()),
            ..MetricsConfig::default()
        };
        assert_eq!(token_warning(3, &with_token), None);
    }
}
