//! The feature-off warnings for `middleware[]` entries, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::MiddlewareEntry;

/// `true` when this build has the `wasm` feature (`type: "wasm"` entries are dispatched). Asserted against the root's `wasm`
/// feature in `src/config/validate/warnings.rs`.
pub const WASM_COMPILED: bool = cfg!(feature = "wasm");

/// `true` when this build has the `rhai` feature (`type: "script"` entries are dispatched). Asserted against the root's
/// `rhai` feature in `src/config/validate/warnings.rs`.
pub const RHAI_COMPILED: bool = cfg!(feature = "rhai");

/// One warning per `middleware[]` entry that this build cannot run, pushed in entry order (an entry has one `type`, so at
/// most one of the two checks fires for it). `i` is the site index used in the message.
pub fn feature_warnings(i: usize, entries: &[MiddlewareEntry], out: &mut Vec<String>) {
    for (j, entry) in entries.iter().enumerate() {
        if !WASM_COMPILED && entry.r#type == "wasm" {
            out.push(wasm_feature_warning_text(i, j));
        }
        if !RHAI_COMPILED && entry.r#type == "script" {
            out.push(rhai_feature_warning_text(i, j));
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
