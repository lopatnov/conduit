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

        for entry in &self.middleware {
            match entry.r#type.as_str() {
                // ── Rhai response scripts ─────────────────────────────────────
                #[cfg(feature = "rhai")]
                "script" => {
                    let phase = entry.phase.as_deref().unwrap_or("request");
                    if phase != "response" {
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
                    if let Some(body_bytes) = outcome.body {
                        // Store the override body in the upstream_response_body
                        // override slot — handled by upstream_response_body_filter.
                        // We signal this via a custom header that the body filter reads.
                        // (Using a header is simpler than extending RequestCtx here.)
                        let _ = resp.insert_header(
                            "x-conduit-wasm-body-override",
                            format!("{}", body_bytes.len()),
                        );
                        // Store body bytes via header value (base64 for safety).
                        use base64::Engine as _;
                        let encoded = base64::engine::general_purpose::STANDARD.encode(&body_bytes);
                        let _ = resp.insert_header("x-conduit-wasm-body-b64", encoded);
                    }
                }

                _ => {}
            }
        }

        Ok(ResponseFilterOutcome::Continue)
    }
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
