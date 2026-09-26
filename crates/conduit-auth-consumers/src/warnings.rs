//! Advisory warnings for `consumers`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::ConsumersConfig;

/// `true` when this build has the `consumers` feature. The root crate asserts at compile time that its own `consumers`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "consumers");

/// `true` when this build has this crate's `jwt` feature (the root's `jwt` forwards into it). Asserted against the root's
/// `jwt` feature the same way as [`COMPILED`].
pub const JWT_COMPILED: bool = cfg!(feature = "jwt");

/// The warning for a `consumers` block that this build ignores because `consumers` is not compiled in; `None` when the
/// feature is compiled in or `consumers` is absent. `i` is the site index used in the message.
pub fn feature_warning(i: usize, config: Option<&ConsumersConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].consumers is configured but Conduit was compiled without the \
         `consumers` feature — consumer authentication will be disabled and every \
         request will bypass it. \
         Recompile with `--features consumers` to enable."
    )
}

/// `consumers` alone doesn't imply `jwt`: a consumer whose only credential is `jwt` (V2) or a `consumers.sharedJwt` block (V3)
/// is silently unreachable without it (see `check_consumer_credentials`/`identify_consumer` in `src/identify.rs`, both
/// `jwt`-gated). Warns when `consumers` is compiled in, `jwt` is not, and such a credential is configured.
pub fn jwt_feature_warning(i: usize, config: Option<&ConsumersConfig>) -> Option<String> {
    if !COMPILED || JWT_COMPILED {
        return None;
    }
    let consumers_cfg = config?;
    let has_shared_jwt = consumers_cfg.shared_jwt.is_some();
    let any_consumer_jwt = consumers_cfg.consumers.iter().any(|c| c.jwt.is_some());
    if has_shared_jwt || any_consumer_jwt {
        Some(jwt_feature_warning_text(i))
    } else {
        None
    }
}

fn jwt_feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].consumers uses `sharedJwt` or a consumer `jwt` credential but \
         Conduit was compiled without the `jwt` feature — those consumers will be \
         permanently unreachable. \
         Recompile with `--features jwt` to enable."
    )
}

/// Warn about consumer-level JWT secrets shorter than 32 bytes. Not feature-gated. `i` is the site index.
pub fn secret_warnings(i: usize, config: &ConsumersConfig, out: &mut Vec<String>) {
    for (j, consumer) in config.consumers.iter().enumerate() {
        if let Some(jwt) = &consumer.jwt {
            if let Some(secret) = &jwt.secret {
                if secret.len() < 32 {
                    out.push(format!(
                        "sites[{i}].consumers.consumers[{j}].jwt.secret is only {} bytes \
                         — minimum recommended length is 32 bytes.",
                        secret.len()
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Consumer, ConsumerJwtConfig};

    fn consumer_with_jwt_secret(secret: &str) -> Consumer {
        Consumer {
            username: "u".to_owned(),
            api_key: None,
            basic_auth: None,
            jwt: Some(ConsumerJwtConfig {
                secret: Some(secret.to_owned()),
                ..ConsumerJwtConfig::default()
            }),
            rate_limit: None,
            headers: None,
        }
    }

    #[test]
    fn texts_are_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].consumers is configured but Conduit was compiled without the \
             `consumers` feature — consumer authentication will be disabled and every \
             request will bypass it. Recompile with `--features consumers` to enable."
        );
        assert_eq!(
            jwt_feature_warning_text(3),
            "sites[3].consumers uses `sharedJwt` or a consumer `jwt` credential but \
             Conduit was compiled without the `jwt` feature — those consumers will be \
             permanently unreachable. Recompile with `--features jwt` to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(3, None), None);
        assert_eq!(jwt_feature_warning(3, None), None);
    }

    #[test]
    fn warns_only_when_configured_and_not_compiled() {
        assert_eq!(
            feature_warning(3, Some(&ConsumersConfig::default())).is_some(),
            !COMPILED
        );
    }

    #[test]
    fn jwt_warning_needs_consumers_without_jwt_and_a_jwt_credential() {
        let with_jwt = ConsumersConfig {
            consumers: vec![consumer_with_jwt_secret("s")],
            ..ConsumersConfig::default()
        };
        assert_eq!(
            jwt_feature_warning(3, Some(&with_jwt)).is_some(),
            COMPILED && !JWT_COMPILED
        );
        assert_eq!(
            jwt_feature_warning(3, Some(&ConsumersConfig::default())),
            None
        );
    }

    #[test]
    fn secret_warnings_are_pinned_and_only_for_short_secrets() {
        let mut out = Vec::new();
        let cfg = ConsumersConfig {
            consumers: vec![
                consumer_with_jwt_secret("short"),
                consumer_with_jwt_secret(&"x".repeat(32)),
            ],
            ..ConsumersConfig::default()
        };
        secret_warnings(3, &cfg, &mut out);
        assert_eq!(
            out,
            vec![
                "sites[3].consumers.consumers[0].jwt.secret is only 5 bytes — minimum \
                 recommended length is 32 bytes."
                    .to_owned()
            ]
        );
    }
}
