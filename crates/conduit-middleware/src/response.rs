//! `MiddlewareResponseFilter` — Phase 7 of the response filter pipeline: runs
//! Rhai and WASM middleware entries that are configured for the response
//! phase.

use conduit_core::filter::response_chain::{ResponseCtx, ResponseFilter, ResponseFilterOutcome};
use pingora_core::Result;
use pingora_http::ResponseHeader;

use crate::config::MiddlewareEntry;

/// Phase 7 — Run Rhai and WASM middleware entries that are configured for the
/// response phase.
///
/// - **WASM** (`type: "wasm"`): if the module exports `on_response(status) -> i32`,
///   it is called here.  The export is optional — modules without it are skipped.
/// - **Rhai** (`type: "script"`, `phase: "response"`): runs the script with
///   `upstream.status`, `upstream.header("Name")`, `response.set_header()`, etc.
///
/// All header mutations collected by the plugins are applied to `resp`.
pub struct MiddlewareResponseFilter {
    pub middleware: Vec<MiddlewareEntry>,
}

impl ResponseFilter for MiddlewareResponseFilter {
    /// Runs a Rhai script or a WASM plugin, which can block (a module read from disk on first load, a slow script), so
    /// the chain holding this filter runs through `block_in_place`. The one thing `apply` provably skips is a
    /// request-phase script; anything else — a response-phase script, a WASM plugin, a type this filter does not know —
    /// counts as blocking, so an entry kind added to `apply` later without touching this stays on the safe side.
    fn may_block(&self) -> bool {
        self.middleware
            .iter()
            .any(|e| !skipped_in_response_phase(e))
    }

    fn apply(
        &self,
        #[cfg_attr(not(any(feature = "rhai", feature = "wasm")), allow(unused_variables))]
        resp: &mut ResponseHeader,
        _req_ctx: &dyn ResponseCtx,
    ) -> Result<ResponseFilterOutcome> {
        // Status and headers are only needed by rhai/wasm plugins.
        #[cfg(any(feature = "rhai", feature = "wasm"))]
        let status = resp.status.as_u16();
        #[cfg(any(feature = "rhai", feature = "wasm"))]
        let headers: std::collections::HashMap<String, String> = resp
            .headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|vs| (k.as_str().to_ascii_lowercase(), vs.to_owned()))
            })
            .collect();

        #[cfg(feature = "wasm")]
        let mut replacement: Option<Vec<u8>> = None;
        for entry in &self.middleware {
            match entry.r#type.as_str() {
                // ── Rhai response scripts ─────────────────────────────────────
                #[cfg(feature = "rhai")]
                "script" => {
                    // The same predicate `may_block` uses, so the two cannot drift apart.
                    if skipped_in_response_phase(entry) {
                        continue;
                    }
                    let Some(ref path) = entry.path else { continue };
                    let outcome = conduit_script_rhai::run_script_response(
                        path,
                        status,
                        headers.clone(),
                        entry.config.as_ref(),
                    );
                    apply_response_mutations(resp, outcome.added_headers, outcome.removed_headers);
                }

                // ── WASM on_response ──────────────────────────────────────────
                #[cfg(feature = "wasm")]
                "wasm" => {
                    let Some(ref path) = entry.path else { continue };
                    let plugin_config = entry
                        .config
                        .as_ref()
                        .and_then(|v| serde_json::to_vec(v).ok())
                        .unwrap_or_default();
                    let ctx = conduit_plugin_wasm::WasmResponseContext {
                        status,
                        headers: headers.clone(),
                        plugin_config,
                    };
                    let outcome = conduit_plugin_wasm::run_wasm_response(ctx, path);
                    apply_response_mutations(resp, outcome.added_headers, outcome.removed_headers);
                    // `conduit_set_response_body` in `on_response` (#379): the last plugin to set
                    // a body wins. Before this was wired the replacement leaked to the client as two
                    // internal `x-conduit-wasm-body-*` headers (one carrying the whole body, base64).
                    if outcome.body.is_some() {
                        replacement = outcome.body.map(|b| b.to_vec());
                    }
                }

                _ => {}
            }
        }

        #[cfg(feature = "wasm")]
        if let Some(body) = replacement {
            return Ok(ResponseFilterOutcome::ReplaceBody(body));
        }
        Ok(ResponseFilterOutcome::Continue)
    }
}

/// A `script` entry whose phase is not `"response"` (the default is `"request"`) is skipped by
/// [`MiddlewareResponseFilter::apply`], so it never runs — and never blocks — in the response phase.
fn skipped_in_response_phase(entry: &MiddlewareEntry) -> bool {
    entry.r#type == "script" && entry.phase.as_deref().unwrap_or("request") != "response"
}

/// Apply header mutations to a Pingora response header.
#[cfg(any(feature = "rhai", feature = "wasm"))]
fn apply_response_mutations(
    resp: &mut ResponseHeader,
    added: Vec<(String, String)>,
    removed: Vec<String>,
) {
    for name in removed {
        resp.remove_header(&name);
    }
    for (name, value) in added {
        let _ = resp.insert_header(name.clone(), value.as_str());
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;

    fn make_resp(status: u16) -> ResponseHeader {
        ResponseHeader::build(StatusCode::from_u16(status).unwrap(), None).unwrap()
    }

    /// Minimal `ResponseCtx` stub — `MiddlewareResponseFilter::apply` binds its
    /// `req_ctx` parameter as `_req_ctx` and never actually reads it, so this
    /// crate doesn't need the root crate's full `RequestCtx` (which isn't
    /// reachable from here anyway — see this crate's `src/lib.rs` doc comment
    /// on the coupling check for this extraction).
    struct DummyCtx;
    impl ResponseCtx for DummyCtx {
        fn cache_age_secs(&self) -> Option<u64> {
            None
        }
    }

    fn dummy_ctx() -> DummyCtx {
        DummyCtx
    }

    // ── MiddlewareResponseFilter ──────────────────────────────────────────────

    #[test]
    fn middleware_response_filter_empty_middleware_is_noop() {
        let filter = MiddlewareResponseFilter { middleware: vec![] };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        let outcome = filter.apply(&mut resp, &ctx).unwrap();
        assert!(matches!(outcome, ResponseFilterOutcome::Continue));
    }

    #[test]
    #[cfg(feature = "rhai")]
    fn middleware_response_filter_request_phase_script_is_skipped() {
        // A script with phase="request" must be skipped in response phase.
        let filter = MiddlewareResponseFilter {
            middleware: vec![MiddlewareEntry {
                r#type: "script".to_owned(),
                path: Some("nonexistent.rhai".to_owned()),
                phase: Some("request".to_owned()), // request phase → skip in response
                config: None,
            }],
        };
        let mut resp = make_resp(200);
        let ctx = dummy_ctx();
        // Must not panic even though the file doesn't exist (script skipped).
        let outcome = filter.apply(&mut resp, &ctx).unwrap();
        assert!(matches!(outcome, ResponseFilterOutcome::Continue));
    }

    /// #379: a plugin that calls `conduit_set_response_body` in `on_response` replaces the body and
    /// must not leak the replacement to the client as internal `x-conduit-wasm-body-*` headers (the
    /// old behaviour, one of them carrying the whole body in base64).
    #[test]
    #[cfg(feature = "wasm")]
    fn wasm_response_body_override_does_not_leak_internal_headers() {
        use std::io::Write as _;
        let wasm = wat::parse_str(
            r#"(module
              (import "conduit" "conduit_set_response_body"
                (func $set_body (param i32 i32)))
              (memory (export "memory") 1)
              (data (i32.const 0) "rewritten body")
              (func (export "on_response") (param i32) (result i32)
                (call $set_body (i32.const 0) (i32.const 14))
                i32.const 0))"#,
        )
        .expect("WAT parse");
        let mut file = tempfile::Builder::new()
            .suffix(".wasm")
            .tempfile()
            .expect("tempfile");
        file.write_all(&wasm).expect("write");
        file.flush().expect("flush");

        let filter = MiddlewareResponseFilter {
            middleware: vec![MiddlewareEntry {
                r#type: "wasm".to_owned(),
                path: Some(file.path().to_string_lossy().into_owned()),
                phase: None,
                config: None,
            }],
        };
        let mut resp = make_resp(500);
        let outcome = filter.apply(&mut resp, &dummy_ctx()).unwrap();
        match outcome {
            ResponseFilterOutcome::ReplaceBody(body) => assert_eq!(body, b"rewritten body"),
            _ => panic!("expected ReplaceBody"),
        }
        for (name, _) in resp.headers.iter() {
            assert!(
                !name.as_str().starts_with("x-conduit-"),
                "internal header leaked to the client: {name}"
            );
        }
    }

    // ── may_block (issue #475) ────────────────────────────────────────────────

    fn entry(kind: &str, phase: Option<&str>) -> MiddlewareEntry {
        MiddlewareEntry {
            r#type: kind.to_owned(),
            path: None,
            phase: phase.map(str::to_owned),
            config: None,
        }
    }

    fn may_block_for(entries: Vec<MiddlewareEntry>) -> bool {
        MiddlewareResponseFilter {
            middleware: entries,
        }
        .may_block()
    }

    /// Only a request-phase script is known not to run here; everything else counts as blocking.
    #[test]
    fn may_block_is_false_only_when_every_entry_is_a_skipped_request_phase_script() {
        assert!(!may_block_for(vec![]), "nothing to run");
        assert!(
            !may_block_for(vec![entry("script", None)]),
            "phase defaults to request"
        );
        assert!(!may_block_for(vec![
            entry("script", Some("request")),
            entry("script", Some("request")),
        ]));

        assert!(may_block_for(vec![entry("script", Some("response"))]));
        assert!(
            may_block_for(vec![entry("wasm", None)]),
            "a WASM plugin runs whatever the phase says"
        );
        assert!(may_block_for(vec![entry("wasm", Some("request"))]));
        assert!(
            may_block_for(vec![entry("something-new", None)]),
            "a type this filter does not know is assumed to block"
        );
        assert!(
            may_block_for(vec![entry("script", Some("request")), entry("wasm", None)]),
            "one blocking entry among skipped ones"
        );
    }

    // ── apply_response_mutations ──────────────────────────────────────────────

    #[test]
    #[cfg(any(feature = "rhai", feature = "wasm"))]
    fn apply_response_mutations_adds_and_removes() {
        let mut resp = make_resp(200);
        resp.insert_header("x-old", "value").unwrap();
        apply_response_mutations(
            &mut resp,
            vec![("x-new".to_owned(), "injected".to_owned())],
            vec!["x-old".to_owned()],
        );
        assert!(resp.headers.get("x-old").is_none(), "x-old must be removed");
        assert_eq!(resp.headers.get("x-new").unwrap(), "injected");
    }

    #[test]
    #[cfg(any(feature = "rhai", feature = "wasm"))]
    fn apply_response_mutations_empty_vecs_is_noop() {
        let mut resp = make_resp(200);
        resp.insert_header("x-keep", "yes").unwrap();
        apply_response_mutations(&mut resp, vec![], vec![]);
        assert_eq!(resp.headers.get("x-keep").unwrap(), "yes");
    }
}
