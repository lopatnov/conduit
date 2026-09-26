//! Config validation for `middleware` entries: called by the root crate's `config::validate`.

use crate::config::MiddlewareEntry;
use conduit_config_core::validation::ValidationError;

pub fn validate_middleware(
    entries: &[MiddlewareEntry],
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (i, entry) in entries.iter().enumerate() {
        let entry_prefix = format!("{prefix}.middleware[{i}]");
        match entry.r#type.as_str() {
            // Both script and wasm require a `path` field.
            "script" | "wasm" => {
                if entry.path.is_none() {
                    errors.push(ValidationError::new(
                        format!("{entry_prefix}.path"),
                        format!(
                            "middleware type {:?} requires a \"path\" field",
                            entry.r#type
                        ),
                    ));
                }
            }
            "ipFilter" | "rateLimit" | "auth" | "headers" => {
                // Built-in types — currently executed via top-level config fields;
                // listed here so the validator accepts them without error.
            }
            other => {
                errors.push(ValidationError::new(
                    format!("{entry_prefix}.type"),
                    format!("unknown middleware type \"{other}\""),
                ));
            }
        }

        // Only "request" (the default) and "response" are ever checked against —
        // any other value (a typo like "resposne") silently falls through both the
        // request-phase and response-phase skip checks and runs in whichever phase
        // it wasn't meant for, or in both. Catch it here instead.
        if let Some(phase) = entry.phase.as_deref() {
            if phase != "request" && phase != "response" {
                errors.push(ValidationError::new(
                    format!("{entry_prefix}.phase"),
                    format!("invalid phase \"{phase}\" — must be \"request\" or \"response\""),
                ));
            }
        }
    }
}
