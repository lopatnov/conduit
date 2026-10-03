//! Advisory warnings for `jwtAuth`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::JwtAuthConfig;

/// `true` when this build has the `jwt` feature. The root crate asserts at compile time that its own `jwt`
/// feature agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "jwt");

/// The warning for `jwtAuth` that this build ignores because `jwt` is not compiled in; `None` when the
/// feature is compiled in or `jwtAuth` is absent.
/// `i` is the site index used in the message.
pub fn feature_warning(i: usize, config: Option<&JwtAuthConfig>) -> Option<String> {
    if COMPILED || config.is_none() {
        return None;
    }
    Some(feature_warning_text(i))
}

fn feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].jwtAuth is configured but Conduit was compiled without the `jwt` \
         feature — JWT authentication will be disabled. \
         Recompile with `--features jwt` to enable."
    )
}

/// Warn when a `jwtAuth.secret` is shorter than the 32-byte minimum for HS256. Not feature-gated.
pub fn secret_warning(i: usize, config: &JwtAuthConfig) -> Option<String> {
    let secret = config.secret.as_ref()?;
    if secret.len() < 32 {
        Some(format!(
            "sites[{i}].jwtAuth.secret is only {} bytes — minimum recommended \
             length is 32 bytes for HS256.  A short secret can be brute-forced. \
             Use a cryptographically random secret of at least 32 bytes.",
            secret.len()
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warning_text_is_pinned() {
        assert_eq!(
            feature_warning_text(3),
            "sites[3].jwtAuth is configured but Conduit was compiled without the `jwt` \
             feature — JWT authentication will be disabled. Recompile with `--features \
             jwt` to enable."
        );
    }

    #[test]
    fn no_warning_when_not_configured() {
        assert_eq!(feature_warning(3, None), None);
    }

    #[test]
    fn warns_only_when_configured_and_not_compiled() {
        assert_eq!(
            feature_warning(3, Some(&JwtAuthConfig::default())).is_some(),
            !COMPILED
        );
    }

    #[test]
    fn secret_warning_is_pinned_and_only_for_short_secrets() {
        let short = JwtAuthConfig {
            secret: Some("short".to_owned()),
            ..JwtAuthConfig::default()
        };
        assert_eq!(
            secret_warning(3, &short),
            Some(
                "sites[3].jwtAuth.secret is only 5 bytes — minimum recommended length is 32 \
                 bytes for HS256.  A short secret can be brute-forced. Use a \
                 cryptographically random secret of at least 32 bytes."
                    .to_owned()
            )
        );
        let long = JwtAuthConfig {
            secret: Some("x".repeat(32)),
            ..JwtAuthConfig::default()
        };
        assert_eq!(secret_warning(3, &long), None);
        assert_eq!(secret_warning(3, &JwtAuthConfig::default()), None);
    }
}
