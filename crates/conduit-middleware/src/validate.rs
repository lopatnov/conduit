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
                // A script that cannot be read or compiled is skipped at run time and the request passes
                // through (fail-open), which for a guard script means the guard is silently off. Say so
                // here, as a warning: the file may legitimately be put in place after validation
                // (issue #566).
                #[cfg(feature = "rhai")]
                if entry.r#type == "script" {
                    if let Some(path) = entry.path.as_deref() {
                        if let Err(why) = conduit_script_rhai::check_script(path) {
                            errors.push(ValidationError::warning(
                                format!("{entry_prefix}.path"),
                                format!(
                                    "script {path:?} {why}; until that is fixed this entry is skipped and requests pass through it"
                                ),
                            ));
                        }
                    }
                }
                // A `wasm` entry needs an engine that can run plugins. If it cannot be built, every plugin
                // would pass requests through (fail-open), so refuse the config or the hot reload instead
                // (issue #626).
                #[cfg(feature = "wasm")]
                if entry.r#type == "wasm" {
                    if let Err(e) = conduit_plugin_wasm::check_engine() {
                        errors.push(ValidationError::new(
                            format!("{entry_prefix}.type"),
                            format!("middleware type \"wasm\" cannot run: {e}"),
                        ));
                    }
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

#[cfg(all(test, feature = "wasm"))]
mod tests {
    use super::*;

    fn entry(kind: &str) -> MiddlewareEntry {
        serde_json::from_value(serde_json::json!({ "type": kind, "path": "plugin.wasm" })).unwrap()
    }

    /// Issue #626: validation builds the plugin engine for a `wasm` entry. With a healthy engine it adds
    /// nothing; the refusal path is covered where the engine is built
    /// (`conduit-plugin-wasm`, `an_engine_that_cannot_be_built_is_reported_not_swallowed`).
    #[test]
    fn a_wasm_entry_with_a_buildable_engine_validates() {
        let mut errors = Vec::new();
        validate_middleware(&[entry("wasm")], "sites[0]", &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(conduit_plugin_wasm::check_engine(), Ok(()));
    }
}

#[cfg(all(test, feature = "rhai"))]
mod rhai_tests {
    use super::*;
    use conduit_config_core::validation::Severity;

    fn script_entry(path: &str) -> MiddlewareEntry {
        serde_json::from_value(serde_json::json!({ "type": "script", "path": path })).unwrap()
    }

    fn validate_one(path: &str) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        validate_middleware(&[script_entry(path)], "sites[0]", &mut errors);
        errors
    }

    /// Issue #566: `validate` used to accept a script that cannot run, and the first request logged it.
    #[test]
    fn a_script_that_compiles_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ok.rhai");
        std::fs::write(&p, "response.status = 403; false").unwrap();
        assert!(validate_one(p.to_str().unwrap()).is_empty());
    }

    #[test]
    fn a_script_that_does_not_compile_is_a_warning_naming_the_position() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.rhai");
        std::fs::write(&p, "this is not rhai (\n").unwrap();
        let errors = validate_one(p.to_str().unwrap());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].severity, Severity::Warning);
        assert!(errors[0].message.contains("does not compile"), "{errors:?}");
        assert!(errors[0].message.contains("line 1"), "{errors:?}");
    }

    #[test]
    fn a_missing_script_file_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("absent.rhai");
        let errors = validate_one(p.to_str().unwrap());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].severity, Severity::Warning);
        assert!(errors[0].message.contains("cannot be read"), "{errors:?}");
        assert!(
            errors[0].message.contains("requests pass through"),
            "{errors:?}"
        );
    }
}
