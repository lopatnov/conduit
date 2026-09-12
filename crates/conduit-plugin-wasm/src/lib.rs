//! WASM plugin middleware for conduit's feature-driven Cargo workspace
//! migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#141](https://github.com/lopatnov/conduit/issues/141)).
//!
//! Plugins are compiled `.wasm` binaries that export a single `on_request`
//! function.  The host exposes a set of `conduit_*` functions for reading the
//! request, inspecting headers, and writing a rejection/redirect response.
//!
//! ## Plugin ABI (17 host functions)
//!
//! **Imports** (namespace `"conduit"`):
//!
//! ### Request read
//! | Function | Description |
//! |---|---|
//! | `conduit_get_method(buf, buf_len) -> i32` | HTTP method; returns bytes written |
//! | `conduit_get_path(buf, buf_len) -> i32` | Request path (without query) |
//! | `conduit_get_query(buf, buf_len) -> i32` | Raw query string (empty if none) |
//! | `conduit_get_uri(buf, buf_len) -> i32` | Full URI: path + "?" + query |
//! | `conduit_get_client_ip(buf, buf_len) -> i32` | Remote IP address |
//! | `conduit_get_request_id(buf, buf_len) -> i32` | X-Request-ID header value |
//! | `conduit_get_header(name, nlen, buf, buf_len) -> i32` | Named header value; -1 if absent |
//! | `conduit_get_header_count() -> i32` | Number of request headers |
//! | `conduit_get_header_names(buf, buf_len) -> i32` | Newline-separated header names |
//! | `conduit_get_plugin_config(buf, buf_len) -> i32` | JSON from `MiddlewareEntry.config` |
//!
//! ### Request mutation
//! | Function | Description |
//! |---|---|
//! | `conduit_set_request_header(name, nlen, val, vlen)` | Add/overwrite request header |
//! | `conduit_remove_request_header(name, nlen)` | Remove a request header |
//!
//! ### Response control (abort path)
//! | Function | Description |
//! |---|---|
//! | `conduit_set_response_status(status)` | Abort with HTTP status code |
//! | `conduit_set_response_header(name, nlen, val, vlen)` | Add header to abort response |
//! | `conduit_set_response_body(body, body_len)` | Set body of abort response |
//! | `conduit_abort_with_redirect(url, url_len)` | Abort with 302 Location redirect |
//!
//! ### Logging
//! | Function | Description |
//! |---|---|
//! | `conduit_log(level, msg, msg_len)` | 0=trace 1=debug 2=info 3=warn 4=error |
//!
//! **Export** (required):
//! ```text
//! on_request() -> i32    // 0 = Continue, 1 = Abort
//! ```
//!
//! **Memory**: plugins must export `"memory"`. All data passes through WASM
//! linear memory; the host never retains pointers after the call.
//!
//! **Error handling**: any error (missing file, compile, link, trap) is logged
//! as a warning and the request passes through (fail-open, same as Rhai).
//!
//! ## Response phase
//!
//! An optional `on_response(status: i32) -> i32` export is called during the
//! response phase via [`run_wasm_response`] — see [`WasmResponseContext`] and
//! [`WasmResponseOutcome`]. Modules without the export are skipped silently
//! (fail-open).
//!
//! ## Scope (#114/#141)
//!
//! This crate has **no `[features]` table** — whether it's compiled into the
//! dependency graph at all is controlled by `conduit-middleware`'s own `wasm`
//! Cargo feature (`wasm = ["dep:lopatnov-conduit-plugin-wasm", ...]`), not by
//! anything declared here. No dependency on `lopatnov-conduit-core` either:
//! this crate implements no chain trait — it only exposes [`run_wasm`]/
//! [`run_wasm_response`] as plain functions taking/returning primitives and
//! outcome enums, which `conduit-middleware`'s `MiddlewareGuard`/
//! `MiddlewareResponseFilter` interpret (see `CONTRIBUTING.md`'s
//! "conduit-core dependency is opt-in, not automatic").

pub mod wasm;

pub use wasm::{
    run_wasm, run_wasm_response, WasmOutcome, WasmRequest, WasmResponseContext, WasmResponseOutcome,
};
