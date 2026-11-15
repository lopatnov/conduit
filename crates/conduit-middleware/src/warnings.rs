//! The feature-off warnings for `middleware[]` entries, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::MiddlewareEntry;

/// `true` when this build has the `wasm` feature (`type: "wasm"` entries are dispatched). Asserted against the root's `wasm`
/// feature in `src/config/validate/warnings.rs`.
pub const WASM_COMPILED: bool = cfg!(feature = "wasm");

/// `true` when this build has the `rhai` feature (`type: "script"` entries are dispatched). Asserted against the root's
/// `rhai` feature in `src/config/validate/warnings.rs`.
pub const RHAI_COMPILED: bool = cfg!(feature = "rhai");

/// `true` when this build has the `ext-proc` feature (`type: "ext_proc"` entries are sent to the external processor).
/// Asserted against the root's `ext-proc` feature in `src/config/validate/mod.rs`.
pub const EXT_PROC_COMPILED: bool = conduit_ext_proc::warnings::COMPILED;

/// One warning per `middleware[]` entry that this build cannot run, pushed in entry order (an entry has one `type`, so at
/// most one of the checks fires for it). `i` is the site index used in the message. An `ext_proc` entry that is not
/// `failOpen` is also a validation error without the feature (`validate::validate_middleware`).
pub fn feature_warnings(i: usize, entries: &[MiddlewareEntry], out: &mut Vec<String>) {
    for (j, entry) in entries.iter().enumerate() {
        if !WASM_COMPILED && entry.r#type == "wasm" {
            out.push(wasm_feature_warning_text(i, j));
        }
        if !RHAI_COMPILED && entry.r#type == "script" {
            out.push(rhai_feature_warning_text(i, j));
        }
        if !EXT_PROC_COMPILED && entry.r#type == "ext_proc" {
            // A config that does not parse is reported by `validate`; the warning is about the feature.
            let fail_open = conduit_ext_proc::ExtProcConfig::from_value(entry.config.as_ref())
                .is_ok_and(|c| c.fail_open());
            out.push(conduit_ext_proc::warnings::feature_warning_text(
                i, j, fail_open,
            ));
        }
    }
}

fn wasm_feature_warning_text(i: usize, j: usize) -> String {
    format!(
        "sites[{i}].middleware[{j}] has type \"wasm\" but Conduit was compiled \
         without the `wasm` feature — this middleware entry will be ignored. \
         Recompile with `--features wasm` to enable."
    )
}

fn rhai_feature_warning_text(i: usize, j: usize) -> String {
    format!(
        "sites[{i}].middleware[{j}] has type \"script\" but Conduit was compiled \
         without the `rhai` feature — this entry will be ignored. \
         Recompile with `--features rhai` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_pinned() {
        assert_eq!(
            wasm_feature_warning_text(3, 5),
            "sites[3].middleware[5] has type \"wasm\" but Conduit was compiled without \
             the `wasm` feature — this middleware entry will be ignored. Recompile with \
             `--features wasm` to enable."
        );
        assert_eq!(
            rhai_feature_warning_text(3, 5),
            "sites[3].middleware[5] has type \"script\" but Conduit was compiled without \
             the `rhai` feature — this entry will be ignored. Recompile with `--features \
             rhai` to enable."
        );
    }

    #[test]
    fn no_entries_no_warnings() {
        let mut out = Vec::new();
        feature_warnings(3, &[], &mut out);
        assert!(out.is_empty());
    }
}

#[cfg(test)]
mod ext_proc_tests {
    use super::*;

    fn entries(config: serde_json::Value) -> Vec<MiddlewareEntry> {
        vec![
            serde_json::from_value(serde_json::json!({ "type": "ext_proc", "config": config }))
                .unwrap(),
        ]
    }

    /// In a build without the feature every `ext_proc` entry is named in a warning (a fail-closed one is also a validation
    /// error), so `conduit features` and the start-up warnings always say which feature is missing; with the feature there
    /// is nothing to warn about.
    #[test]
    fn an_ext_proc_entry_warns_in_a_build_without_the_feature() {
        let warn = |config: serde_json::Value| {
            let mut out = Vec::new();
            feature_warnings(2, &entries(config), &mut out);
            out
        };
        let open = warn(serde_json::json!({ "grpc": "grpc://h:1", "failOpen": true }));
        let closed = warn(serde_json::json!({ "grpc": "grpc://h:1" }));
        if EXT_PROC_COMPILED {
            assert!(open.is_empty() && closed.is_empty(), "{open:?} {closed:?}");
        } else {
            assert_eq!(open.len(), 1, "{open:?}");
            assert_eq!(closed.len(), 1, "{closed:?}");
            assert!(open[0].contains("failOpen: true"), "{}", open[0]);
            assert!(closed[0].contains("is refused"), "{}", closed[0]);
            for w in open.iter().chain(&closed) {
                assert!(
                    w.starts_with("sites[2].middleware[0] has type \"ext_proc\""),
                    "{w}"
                );
                assert!(w.contains("`--features ext-proc`"), "{w}");
            }
        }
    }
}
