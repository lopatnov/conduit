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
                check_runnable(entry, &entry_prefix, errors);
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

/// What `validate` can say about a `script` or `wasm` entry beyond its shape: whether it can run at all.
/// (A separate function: it keeps `validate_middleware` under the complexity limit.)
#[cfg_attr(
    not(any(feature = "rhai", feature = "wasm")),
    allow(unused_variables, clippy::ptr_arg)
)]
fn check_runnable(entry: &MiddlewareEntry, entry_prefix: &str, errors: &mut Vec<ValidationError>) {
    // A script that cannot be read or compiled is skipped at run time and the request passes through
    // (fail-open), which for a guard script means the guard is silently off. Say so here, as a warning:
    // the file may legitimately be put in place after validation (issue #566).
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
    // A `wasm` entry needs an engine that can run plugins. If it cannot be built, every plugin would pass
    // requests through (fail-open), so refuse the config or the hot reload instead (issue #626).
    #[cfg(feature = "wasm")]
    if entry.r#type == "wasm" {
        match conduit_plugin_wasm::check_engine() {
            Err(e) => errors.push(ValidationError::new(
                format!("{entry_prefix}.type"),
                format!("middleware type \"wasm\" cannot run: {e}"),
            )),
            // With a working engine, the file itself: a module that cannot be read, compiled or linked is
            // skipped at run time and the request passes through, so an access check in it is silently off.
            // A warning, as for scripts: the file may be put in place after validation (issue #565).
            Ok(()) => {
                if let Some(path) = entry.path.as_deref() {
                    if let Err(problem) = conduit_plugin_wasm::check_module(path) {
                        let consequence = match problem {
                            conduit_plugin_wasm::ModuleProblem::Unusable(_) => {
                                "until that is fixed this entry is skipped and requests pass through it"
                            }
                            conduit_plugin_wasm::ModuleProblem::Degraded(_) => {
                                "the plugin still runs, but it does not behave as written"
                            }
                        };
                        errors.push(ValidationError::warning(
                            format!("{entry_prefix}.path"),
                            format!("plugin {path:?} {}; {consequence}", problem.message()),
                        ));
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "wasm"))]
mod tests {
    use super::*;
    use conduit_config_core::validation::Severity;

    fn entry(path: &str) -> MiddlewareEntry {
        serde_json::from_value(serde_json::json!({ "type": "wasm", "path": path })).unwrap()
    }

    fn validate_one(path: &str) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        validate_middleware(&[entry(path)], "sites[0]", &mut errors);
        errors
    }

    /// The smallest module the host accepts: `on_request` returns 0 (`(module (func (export "on_request")
    /// (result i32) i32.const 0))`, assembled by hand so that this crate needs no WAT parser).
    const MINIMAL_PLUGIN: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type: () -> i32
        0x03, 0x02, 0x01, 0x00, // one function of that type
        0x07, 0x0e, 0x01, 0x0a, b'o', b'n', b'_', b'r', b'e', b'q', b'u', b'e', b's', b't', 0x00,
        0x00, // export "on_request"
        0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x00, 0x0b, // body: i32.const 0
    ];

    /// Issue #626: validation builds the plugin engine for a `wasm` entry. With a healthy engine and a
    /// plugin that can run it adds nothing; the refusal path is covered where the engine is built
    /// (`conduit-plugin-wasm`, `an_engine_that_cannot_be_built_is_reported_not_swallowed`).
    #[test]
    fn a_wasm_entry_with_a_buildable_engine_and_a_good_plugin_validates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ok.wasm");
        std::fs::write(&p, MINIMAL_PLUGIN).unwrap();
        let errors = validate_one(p.to_str().unwrap());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(conduit_plugin_wasm::check_engine(), Ok(()));
    }

    /// A plugin that imports a function reading its memory but exports none still runs (its reads come back empty),
    /// so the warning must not say that the entry is skipped.
    #[test]
    fn a_plugin_without_an_exported_memory_is_a_warning_that_says_it_still_runs() {
        let mut module = Vec::new();
        module.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
        // types: (i32, i32) -> i32 and () -> i32
        module.extend_from_slice(&[
            0x01, 0x0b, 0x02, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, 0x60, 0x00, 0x01, 0x7f,
        ]);
        // one import, `conduit.conduit_get_path`, of the first type
        module.extend_from_slice(&[0x02, 0x1c, 0x01, 0x07]);
        module.extend_from_slice(b"conduit");
        module.push(0x10);
        module.extend_from_slice(b"conduit_get_path");
        module.extend_from_slice(&[0x00, 0x00]);
        // one function of the second type, exported as `on_request` (function index 1: the import is 0)
        module.extend_from_slice(&[0x03, 0x02, 0x01, 0x01, 0x07, 0x0e, 0x01, 0x0a]);
        module.extend_from_slice(b"on_request");
        module.extend_from_slice(&[0x00, 0x01]);
        // body: i32.const 0
        module.extend_from_slice(&[0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x00, 0x0b]);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nomem.wasm");
        std::fs::write(&p, module).unwrap();
        let errors = validate_one(p.to_str().unwrap());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].severity, Severity::Warning);
        assert!(
            errors[0].message.contains("does not export a memory"),
            "{errors:?}"
        );
        assert!(errors[0].message.contains("still runs"), "{errors:?}");
        assert!(!errors[0].message.contains("skipped"), "{errors:?}");
    }

    /// Issue #565: `validate` used to accept a plugin that cannot run; at run time it is skipped.
    #[test]
    fn a_missing_plugin_file_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("absent.wasm");
        let errors = validate_one(p.to_str().unwrap());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].severity, Severity::Warning);
        assert!(errors[0].message.contains("cannot be read"), "{errors:?}");
        assert!(
            errors[0].message.contains("requests pass through"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_plugin_that_is_not_a_module_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.wasm");
        std::fs::write(&p, b"not webassembly").unwrap();
        let errors = validate_one(p.to_str().unwrap());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].severity, Severity::Warning);
        assert!(
            errors[0].message.contains("not a valid WebAssembly module"),
            "{errors:?}"
        );
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
