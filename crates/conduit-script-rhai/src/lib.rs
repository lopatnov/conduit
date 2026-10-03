//! Rhai scripting middleware for conduit's feature-driven Cargo workspace
//! migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#141](https://github.com/lopatnov/conduit/issues/141)).
//!
//! Exposes a sandboxed [`run_script`] function that executes a Rhai script
//! against the current request and returns a decision: continue the pipeline
//! or abort with a custom response. [`run_script_response`] does the same for
//! the response phase (`phase: "response"` middleware entries).
//!
//! # Script API (request phase)
//!
//! Scripts receive two objects:
//!
//! - **`request`** — read-only view of the incoming request:
//!   - `request.path` → `String`
//!   - `request.method` → `String`
//!   - `request.query` → `String` (empty when no query)
//!   - `request.header("Name")` → `String` (empty when absent, case-insensitive)
//!
//! - **`response`** — the response to send when aborting:
//!   - `response.status` → `int` (get/set, default `200`)
//!   - `response.body` → `String` (get/set, default `""`)
//!   - `response.header("Name", "Value")` — append a response header
//!
//! A script that returns `true` (or ends without an explicit `false`) passes
//! the request through to the next pipeline stage.  Returning `false` sends
//! the response object and stops the pipeline.
//!
//! # Example
//!
//! ```rhai
//! let token = request.header("Authorization");
//! if token == "" {
//!     response.status = 401;
//!     response.header("WWW-Authenticate", "Bearer");
//!     return false;
//! }
//! true
//! ```
//!
//! # Script API (response phase)
//!
//! - `response` — a mutable builder: `response.status` (read),
//!   `response.set_header()`, `response.remove_header()`
//! - `upstream` — read-only view: `upstream.status`, `upstream.header("Name")`
//! - `config` — same as request phase
//!
//! ## Scope (#114/#141)
//!
//! This crate has **no `[features]` table** — whether it's compiled into the
//! dependency graph at all is controlled by `conduit-middleware`'s own `rhai`
//! Cargo feature (`rhai = ["dep:lopatnov-conduit-script-rhai"]`), not by
//! anything declared here. No dependency on `lopatnov-conduit-core` either:
//! this crate implements no chain trait — it only exposes [`run_script`]/
//! [`run_script_response`] as plain functions taking/returning primitives and
//! outcome enums, which `conduit-middleware`'s `MiddlewareGuard`/
//! `MiddlewareResponseFilter` interpret (see `CONTRIBUTING.md`'s
//! "conduit-core dependency is opt-in, not automatic"). `ScriptRequest`/
//! `ScriptResponse`/`ScriptResponseBuilder`/`ScriptUpstreamView` are
//! `pub(crate)` — Rhai's `register_type_with_name`/`register_fn` need
//! `'static + Clone + Send + Sync`, not `pub` visibility, and none of the
//! four has a caller outside this crate.

pub mod script;

pub use script::{run_script, run_script_response, ScriptOutcome, ScriptResponseOutcome};
