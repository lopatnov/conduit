//! Config validation for `redirects`: called by the root crate's `config::validate`.

use crate::config::RedirectRule;
use conduit_config_core::validation::ValidationError;

pub fn validate_redirect_rules(
    redirects: &[RedirectRule],
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (j, rule) in redirects.iter().enumerate() {
        if let Some(status) = rule.status {
            if !matches!(status, 301 | 302 | 307 | 308) {
                errors.push(ValidationError::new(
                    format!("{prefix}.redirects[{j}].status"),
                    format!("Invalid redirect status {status} — must be 301, 302, 307, or 308"),
                ));
            }
        }
    }
}
