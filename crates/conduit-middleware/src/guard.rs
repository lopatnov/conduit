//! `MiddlewareGuard` — the request-phase Rhai/WASM dispatcher.
//!
//! Dispatches by `entry.r#type`. This is a deliberate closed `match`, not a
//! `MiddlewarePlugin` trait/registry — see the crate root doc comment for why
//! (issue #114/#141, and the correction recorded on issue #141 itself plus
//! the follow-up issue #392 tracking it as a deferred question).

#[cfg(any(feature = "rhai", feature = "wasm", feature = "ext-proc"))]
use std::sync::atomic::Ordering;

use async_trait::async_trait;
#[cfg(any(feature = "rhai", feature = "ext-proc"))]
use bytes::Bytes;
use conduit_core::filter::chain::{FilterContext, FilterOutcome, RequestFilter};
use pingora_core::Result;

use crate::config::MiddlewareEntry;

/// Executes all middleware entries in the order they appear in `site.middleware`.
///
/// Dispatches by `entry.r#type`:
/// - `"script"` → Rhai scripting (always available)
/// - `"wasm"`   → WASM plugin (requires `--features wasm`; skipped with a warning if disabled)
/// - `"ext_proc"` → external processor over gRPC, Envoy's protocol (requires `--features ext-proc`; a fail-closed entry is
///   refused at validation time in a build without it)
/// - other      → skipped (unknown types are rejected at config validation time)
///
/// This struct replaces the former `ScriptGuard` so that Rhai and WASM entries
/// interleave freely in the declared order.
pub struct MiddlewareGuard {
    pub middleware: Vec<MiddlewareEntry>,
    /// Request path forwarded to scripts/plugins.
    pub req_path: String,
    pub method: String,
    pub query: String,
    /// Lower-cased header map forwarded to scripts/plugins.
    pub headers: std::collections::HashMap<String, String>,
    /// Remote client IP (used by WASM plugins).
    pub client_ip: String,
}

/// Backward-compatible type alias — existing code that names `ScriptGuard`
/// still compiles.  New code should use `MiddlewareGuard` directly.
pub type ScriptGuard = MiddlewareGuard;

#[async_trait]
impl RequestFilter for MiddlewareGuard {
    async fn apply<'a>(
        &self,
        #[cfg_attr(
            not(any(feature = "rhai", feature = "wasm", feature = "ext-proc")),
            allow(unused_variables)
        )]
        ctx: &mut FilterContext<'a>,
    ) -> Result<FilterOutcome> {
        // The header view the next entry sees. A WASM entry's added/removed request headers are
        // folded in as it returns, so a later Rhai or WASM entry reads what the earlier one
        // left, not the original request (issue #395).
        #[cfg(any(feature = "rhai", feature = "wasm", feature = "ext-proc"))]
        #[cfg_attr(not(any(feature = "wasm", feature = "ext-proc")), allow(unused_mut))]
        let mut headers = self.headers.clone();
        for entry in &self.middleware {
            match entry.r#type.as_str() {
                // ── Rhai scripting ────────────────────────────────────────────
                #[cfg(feature = "rhai")]
                "script" => {
                    if entry.phase.as_deref() == Some("response") {
                        continue;
                    }
                    let Some(ref path) = entry.path else { continue };
                    if apply_rhai_entry(self, &headers, path, entry, ctx).await? {
                        return Ok(FilterOutcome::Handled);
                    }
                }

                // ── WASM plugins ──────────────────────────────────────────────
                #[cfg(feature = "wasm")]
                "wasm" => {
                    let Some(ref path) = entry.path else { continue };
                    if apply_wasm_entry(self, &mut headers, path, entry, ctx).await? {
                        return Ok(FilterOutcome::Handled);
                    }
                }

                // ── External processor (request headers) ──────────────────────
                #[cfg(feature = "ext-proc")]
                "ext_proc" => {
                    let ended = apply_ext_proc_entry(&mut headers, entry, ctx).await?;
                    if ended {
                        return Ok(FilterOutcome::Handled);
                    }
                }

                #[cfg(not(feature = "ext-proc"))]
                "ext_proc" => {
                    // Only a `failOpen: true` entry gets here: validation refuses a fail-closed one in this build.
                    tracing::warn!(
                        "ext_proc middleware entry ignored — rebuild with --features ext-proc"
                    );
                }

                // ── Feature disabled: warn and skip ───────────────────────────
                #[cfg(not(feature = "wasm"))]
                "wasm" => {
                    tracing::warn!(
                        path = entry.path.as_deref().unwrap_or("<none>"),
                        "WASM middleware entry ignored — rebuild with --features wasm"
                    );
                }

                // ── Unknown types (rejected at validation time) ────────────────
                _ => {}
            }
        }
        Ok(FilterOutcome::Continue)
    }
}

/// Run a Rhai script entry and return `true` if the request was aborted.
#[cfg(feature = "rhai")]
async fn apply_rhai_entry<'a>(
    guard: &MiddlewareGuard,
    headers: &std::collections::HashMap<String, String>,
    path: &str,
    entry: &MiddlewareEntry,
    ctx: &mut FilterContext<'a>,
) -> Result<bool> {
    // `run_script` may read the script file from disk on first call (subsequent
    // calls use the AST cache).  Use `block_in_place` so the Tokio scheduler
    // knows this thread may block and can temporarily move other tasks elsewhere.
    let outcome = tokio::task::block_in_place(|| {
        conduit_script_rhai::run_script(
            path,
            &guard.req_path,
            &guard.method,
            &guard.query,
            headers.clone(),
            entry.config.as_ref(),
        )
    });
    match outcome {
        conduit_script_rhai::ScriptOutcome::Continue => Ok(false),
        conduit_script_rhai::ScriptOutcome::Abort {
            status,
            body,
            extra_headers,
        } => {
            let mut all = ctx.extra_headers.to_vec();
            all.extend(extra_headers);
            conduit_core::handler::response::write_response(
                ctx.session,
                status,
                "text/plain",
                Bytes::from(body),
                &all,
            )
            .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            Ok(true)
        }
    }
}

/// Run a WASM plugin entry and return `true` if the request was aborted.
#[cfg(feature = "wasm")]
async fn apply_wasm_entry<'a>(
    guard: &MiddlewareGuard,
    headers: &mut std::collections::HashMap<String, String>,
    path: &str,
    entry: &MiddlewareEntry,
    ctx: &mut FilterContext<'a>,
) -> Result<bool> {
    let plugin_config = entry
        .config
        .as_ref()
        .and_then(|v| serde_json::to_vec(v).ok())
        .unwrap_or_default();
    let header_names: Vec<String> = headers.keys().cloned().collect();
    let request_id = ctx
        .session
        .req_header()
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    let request = conduit_plugin_wasm::WasmRequest {
        method: guard.method.clone(),
        path: guard.req_path.clone(),
        query: guard.query.clone(),
        client_ip: guard.client_ip.clone(),
        headers: headers.clone(),
        header_names,
        request_id,
        plugin_config,
    };

    // `run_wasm` reads the .wasm file from disk on first call.
    // Use block_in_place so Tokio can schedule around the I/O.
    let wasm_outcome = tokio::task::block_in_place(|| conduit_plugin_wasm::run_wasm(request, path));
    match wasm_outcome {
        conduit_plugin_wasm::WasmOutcome::Continue {
            added_headers,
            removed_headers,
        } => {
            for (name, val) in added_headers {
                headers.insert(name.to_ascii_lowercase(), val.clone());
                let _ = ctx.session.req_header_mut().insert_header(name, val);
            }
            for name in removed_headers {
                headers.remove(&name.to_ascii_lowercase());
                ctx.session.req_header_mut().remove_header(&name);
            }
            Ok(false)
        }
        conduit_plugin_wasm::WasmOutcome::Abort {
            status,
            body,
            headers,
        } => {
            let mut all = ctx.extra_headers.to_vec();
            all.extend(headers);
            conduit_core::handler::response::write_response(
                ctx.session,
                status,
                "application/octet-stream",
                body,
                &all,
            )
            .await?;
            ctx.inflight.fetch_sub(1, Ordering::Relaxed);
            Ok(true)
        }
    }
}

/// Send the request headers to an `ext_proc` entry's processor and apply the answer. Returns `true` if the request was
/// ended (the processor's immediate response, or the refusal of a fail-closed entry whose processor failed).
#[cfg(feature = "ext-proc")]
async fn apply_ext_proc_entry<'a>(
    headers: &mut std::collections::HashMap<String, String>,
    entry: &MiddlewareEntry,
    ctx: &mut FilterContext<'a>,
) -> Result<bool> {
    use conduit_ext_proc::{ExtProcClient, ExtProcConfig, RequestInfo, Verdict};

    // `validate` refuses a config that does not parse, so this is a config that changed under a running guard or a build
    // that skipped validation: fail closed, never let the request through unchecked.
    let config = match ExtProcConfig::from_value(entry.config.as_ref()) {
        Ok(config) => config,
        Err(message) => {
            tracing::warn!(target: "conduit::ext_proc", %message, "invalid ext_proc config; the request is refused");
            return refuse(
                ctx,
                503,
                Bytes::from_static(b"external processing failed\n"),
                Vec::new(),
            )
            .await;
        }
    };
    let client = ExtProcClient::new(&config);

    let end_of_stream = ctx.session.is_body_empty();
    let secure = ctx.session.digest().is_some_and(|d| d.ssl_digest.is_some());
    let verdict = {
        let req = ctx.session.req_header();
        let authority = req
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .or_else(|| req.uri.authority().map(|a| a.as_str()))
            .unwrap_or("");
        let path = req
            .uri
            .path_and_query()
            .map_or_else(|| req.uri.path(), |pq| pq.as_str());
        client
            .process_request_headers(&RequestInfo {
                method: req.method.as_str(),
                scheme: if secure { "https" } else { "http" },
                authority,
                path,
                headers: &req.headers,
                end_of_stream,
            })
            .await
    };

    match verdict {
        Verdict::Continue(changes) => {
            apply_header_changes(ctx.session.req_header_mut(), headers, &changes);
            Ok(false)
        }
        Verdict::Respond(answer) => {
            let extra = text_headers(&answer);
            refuse(ctx, answer.status, answer.body, extra).await
        }
    }
}

/// End the request with `status`: the headers of the site (CORS, security) plus `extra`, which win by name.
#[cfg(feature = "ext-proc")]
async fn refuse<'a>(
    ctx: &mut FilterContext<'a>,
    status: u16,
    body: Bytes,
    extra: Vec<(String, String)>,
) -> Result<bool> {
    let mut all = ctx.extra_headers.to_vec();
    all.extend(extra);
    if matches!(status, 204 | 205 | 304) {
        // These have no body, and a server must not send `Content-Length` with a 204 (RFC 9110 sections 8.6, 15.3.5,
        // 15.3.6, 15.4.5), which `write_response` always does: the header is built here without it.
        let resp = bodyless_response(status, all)?;
        ctx.session
            .write_response_header(Box::new(resp), true)
            .await?;
    } else {
        conduit_core::handler::response::write_response(
            ctx.session,
            status,
            "text/plain",
            body,
            &all,
        )
        .await?;
    }
    ctx.inflight.fetch_sub(1, Ordering::Relaxed);
    Ok(true)
}

/// The headers of a processor's immediate response as strings. A value that is not text cannot be passed on as a response
/// header here (the writer takes strings), so it is left out.
#[cfg(feature = "ext-proc")]
fn text_headers(answer: &conduit_ext_proc::DirectResponse) -> Vec<(String, String)> {
    answer
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_owned(), v.to_owned()))
        })
        .collect()
}

/// The header of a response that has no body (204, 205, 304): `headers` without a `Content-Length`, which `write_response`
/// would add. A header the writer cannot take is left out, as `write_response` does.
#[cfg(feature = "ext-proc")]
fn bodyless_response(
    status: u16,
    headers: Vec<(String, String)>,
) -> Result<pingora_http::ResponseHeader> {
    let mut resp = pingora_http::ResponseHeader::build(status, Some(headers.len()))?;
    for (name, value) in headers {
        let _ = resp.insert_header(name, value);
    }
    Ok(resp)
}

/// Apply a processor's validated header changes to the request, and to the lower-cased view later entries read.
#[cfg(feature = "ext-proc")]
fn apply_header_changes(
    request: &mut pingora_http::RequestHeader,
    view: &mut std::collections::HashMap<String, String>,
    changes: &conduit_ext_proc::HeaderChanges,
) {
    use conduit_ext_proc::SetMode;

    for set in &changes.set {
        let present = request.headers.contains_key(&set.name);
        let write = match set.mode {
            SetMode::Append | SetMode::Overwrite => true,
            SetMode::AddIfAbsent => !present,
            SetMode::OverwriteIfExists => present,
        };
        if !write {
            continue;
        }
        let applied = if set.mode == SetMode::Append {
            request.append_header(&set.name, set.value.clone())
        } else {
            request
                .insert_header(&set.name, set.value.clone())
                .map(|()| true)
        };
        if applied.is_err() {
            continue;
        }
        let key = set.name.as_str().to_owned();
        if let Ok(value) = set.value.to_str() {
            match (set.mode, view.get_mut(&key)) {
                (SetMode::Append, Some(existing)) => {
                    existing.push_str(", ");
                    existing.push_str(value);
                }
                _ => {
                    view.insert(key, value.to_owned());
                }
            }
        } else {
            // A value that is not text cannot be put in the view, and the old one is no longer true: later entries must
            // not decide on a stale header, so the header is left out of the view (it is not there for them).
            view.remove(&key);
        }
    }
    for name in &changes.remove {
        request.remove_header(name);
        view.remove(name.as_str());
    }
}

#[cfg(all(test, feature = "ext-proc"))]
mod ext_proc_tests {
    use super::*;

    #[test]
    fn a_bodyless_response_has_the_given_headers_and_no_content_length() {
        let headers = vec![
            ("x-site".to_owned(), "a".to_owned()),
            ("bad name".to_owned(), "dropped".to_owned()),
        ];
        let resp = bodyless_response(204, headers).unwrap();
        assert_eq!(resp.status.as_u16(), 204);
        assert_eq!(resp.headers.get("x-site").unwrap(), "a");
        assert!(resp.headers.get("content-length").is_none());
        assert_eq!(
            resp.headers.len(),
            1,
            "a header the writer cannot take is left out"
        );
        assert!(
            bodyless_response(99, Vec::new()).is_err(),
            "a status outside 100 to 999 is an error, not a panic"
        );
    }

    #[test]
    fn only_text_values_of_an_immediate_response_become_response_headers() {
        let headers = vec![
            (
                http::HeaderName::from_static("x-text"),
                http::HeaderValue::from_static("yes"),
            ),
            (
                http::HeaderName::from_static("x-binary"),
                http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
            ),
        ];
        let answer = conduit_ext_proc::DirectResponse {
            status: 403,
            headers,
            body: Bytes::new(),
        };
        assert_eq!(
            text_headers(&answer),
            vec![("x-text".to_owned(), "yes".to_owned())]
        );
    }

    use conduit_ext_proc::{HeaderChanges, HeaderSet, SetMode};
    use http::{HeaderName, HeaderValue};
    use pingora_http::RequestHeader;
    use std::collections::HashMap;

    fn set(name: &str, value: &str, mode: SetMode) -> HeaderSet {
        HeaderSet {
            name: HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value: HeaderValue::from_str(value).unwrap(),
            mode,
        }
    }

    fn request() -> RequestHeader {
        let mut req = RequestHeader::build("GET", b"/", None).unwrap();
        req.append_header("x-keep", "1").unwrap();
        req.append_header("x-multi", "a").unwrap();
        req.append_header("x-multi", "b").unwrap();
        req.append_header("x-user", "ann").unwrap();
        req
    }

    fn values(req: &RequestHeader, name: &str) -> Vec<String> {
        req.headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn every_set_mode_does_what_its_name_says() {
        let mut req = request();
        let mut view = HashMap::from([("x-multi".to_owned(), "a, b".to_owned())]);
        let changes = HeaderChanges {
            set: vec![
                set("x-multi", "c", SetMode::Append),
                set("x-user", "bob", SetMode::Overwrite),
                set("x-user", "carol", SetMode::AddIfAbsent), // present: no change
                set("x-new", "1", SetMode::AddIfAbsent),      // absent: added
                set("x-ghost", "1", SetMode::OverwriteIfExists), // absent: not created
                set("x-keep", "2", SetMode::OverwriteIfExists), // present: replaced
            ],
            remove: Vec::new(),
        };
        apply_header_changes(&mut req, &mut view, &changes);

        assert_eq!(values(&req, "x-multi"), ["a", "b", "c"]);
        assert_eq!(values(&req, "x-user"), ["bob"]);
        assert_eq!(values(&req, "x-new"), ["1"]);
        assert!(req.headers.get("x-ghost").is_none());
        assert_eq!(values(&req, "x-keep"), ["2"]);

        // the view later entries read follows
        assert_eq!(view["x-multi"], "a, b, c");
        assert_eq!(view["x-user"], "bob");
        assert_eq!(view["x-new"], "1");
        assert!(!view.contains_key("x-ghost"));
    }

    #[test]
    fn removals_apply_after_the_sets_and_clear_every_value() {
        let mut req = request();
        let mut view = HashMap::from([
            ("x-multi".to_owned(), "a, b".to_owned()),
            ("x-user".to_owned(), "ann".to_owned()),
        ]);
        let changes = HeaderChanges {
            set: vec![set("x-user", "bob", SetMode::Overwrite)],
            remove: vec![
                HeaderName::from_static("x-user"),
                HeaderName::from_static("x-multi"),
            ],
        };
        apply_header_changes(&mut req, &mut view, &changes);
        assert!(
            req.headers.get("x-user").is_none(),
            "a removal wins over a set of the same name"
        );
        assert!(req.headers.get("x-multi").is_none());
        assert!(view.is_empty());
        assert_eq!(values(&req, "x-keep"), ["1"]);
    }

    #[test]
    fn a_header_overwritten_with_a_non_text_value_leaves_the_view_so_later_entries_do_not_read_a_stale_one(
    ) {
        let mut req = request();
        let mut view = HashMap::from([("x-user".to_owned(), "ann".to_owned())]);
        let binary = HeaderSet {
            name: HeaderName::from_static("x-user"),
            value: HeaderValue::from_bytes(b"\xff\xfe").unwrap(),
            mode: SetMode::Overwrite,
        };
        let changes = HeaderChanges {
            set: vec![binary],
            remove: Vec::new(),
        };
        apply_header_changes(&mut req, &mut view, &changes);
        assert_eq!(req.headers.get("x-user").unwrap().as_bytes(), b"\xff\xfe");
        assert!(
            !view.contains_key("x-user"),
            "the view must not keep the old value: {view:?}"
        );
    }
}
