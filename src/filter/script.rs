//! Rhai scripting middleware — Phase 4.1.
//!
//! Exposes a sandboxed [`run_script`] function that executes a Rhai script
//! against the current request and returns a decision: continue the pipeline
//! or abort with a custom response.
//!
//! # Script API
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

use std::collections::HashMap;
use std::sync::OnceLock;

use dashmap::DashMap;
use rhai::{Engine, Scope, AST};

// ── Custom Rhai types ─────────────────────────────────────────────────────────

/// Read-only view of the HTTP request, exposed to Rhai scripts as `request`.
#[derive(Debug, Clone)]
pub struct ScriptRequest {
    pub path: String,
    pub method: String,
    pub query: String,
    /// Header map — keys are stored lower-cased for case-insensitive look-up.
    pub headers: HashMap<String, String>,
}

impl ScriptRequest {
    /// Return the value of request header `name`, or an empty string if absent.
    ///
    /// Look-up is case-insensitive.
    pub fn header(&mut self, name: &str) -> String {
        self.headers
            .get(&name.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }
}

/// Mutable response builder, exposed to Rhai scripts as `response`.
///
/// Scripts set `status`, `body`, and/or call `header()` before returning
/// `false` to abort the pipeline.
#[derive(Debug, Clone)]
pub struct ScriptResponse {
    pub status: i64,
    pub body: String,
    pub extra_headers: Vec<(String, String)>,
}

impl Default for ScriptResponse {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptResponse {
    pub fn new() -> Self {
        Self {
            status: 200,
            body: String::new(),
            extra_headers: Vec::new(),
        }
    }

    /// Append a response header.
    pub fn header(&mut self, name: &str, value: &str) {
        self.extra_headers.push((name.to_owned(), value.to_owned()));
    }
}

// ── Script engine singleton ───────────────────────────────────────────────────

/// Rhai engine initialised once per process with the Conduit API registered.
static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Compiled AST cache — keyed by script path string.
///
/// Scripts are compiled once and cached indefinitely.  If the path changes
/// between config reloads the new path will get a fresh compilation entry.
static AST_CACHE: OnceLock<DashMap<String, AST>> = OnceLock::new();

fn ast_cache() -> &'static DashMap<String, AST> {
    AST_CACHE.get_or_init(DashMap::new)
}

/// Return a reference to the shared Rhai engine with the Conduit API
/// registered.  The engine is constructed at most once per process.
fn engine() -> &'static Engine {
    ENGINE.get_or_init(|| {
        let mut eng = Engine::new();

        // Register ScriptRequest ────────────────────────────────────────
        eng.register_type_with_name::<ScriptRequest>("Request");
        eng.register_fn("header", ScriptRequest::header);
        eng.register_get("path", |r: &mut ScriptRequest| r.path.clone());
        eng.register_get("method", |r: &mut ScriptRequest| r.method.clone());
        eng.register_get("query", |r: &mut ScriptRequest| r.query.clone());

        // Register ScriptResponse ───────────────────────────────────────
        eng.register_type_with_name::<ScriptResponse>("Response");
        eng.register_get_set(
            "status",
            |r: &mut ScriptResponse| r.status,
            |r: &mut ScriptResponse, s: i64| r.status = s,
        );
        eng.register_get_set(
            "body",
            |r: &mut ScriptResponse| r.body.clone(),
            |r: &mut ScriptResponse, b: String| r.body = b,
        );
        eng.register_fn("header", ScriptResponse::header);

        eng
    })
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Outcome of executing a Rhai script against the current request.
#[derive(Debug)]
pub enum ScriptOutcome {
    /// Pipeline continues — the script returned `true` (or a truthy value).
    Continue,
    /// Pipeline aborted — the script returned `false`.
    ///
    /// The caller should send `status` with `body` and `extra_headers` back
    /// to the client.
    Abort {
        status: u16,
        body: String,
        extra_headers: Vec<(String, String)>,
    },
}

/// Execute the Rhai script at `script_path` against `req`.
///
/// - Compiles the script on first call and caches the AST.
/// - Returns [`ScriptOutcome::Continue`] when the script returns any truthy
///   value or completes without an explicit `return false`.
/// - Returns [`ScriptOutcome::Abort`] when the script returns `false`.
/// - On engine or I/O errors, logs a warning and returns `Continue` so that
///   a broken script does not take down the server.
pub fn run_script(
    script_path: &str,
    path: &str,
    method: &str,
    query: &str,
    headers: HashMap<String, String>,
) -> ScriptOutcome {
    let ast = match get_or_compile(script_path) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(script = script_path, "Rhai compile error: {e}");
            return ScriptOutcome::Continue;
        }
    };

    let eng = engine();
    let mut scope = Scope::new();

    let req = ScriptRequest {
        path: path.to_owned(),
        method: method.to_owned(),
        query: query.to_owned(),
        headers,
    };
    let resp = ScriptResponse::new();

    scope.push("request", req);
    scope.push("response", resp);

    let result = eng.eval_ast_with_scope::<rhai::Dynamic>(&mut scope, &ast);
    match result {
        Err(e) => {
            tracing::warn!(script = script_path, "Rhai runtime error: {e}");
            ScriptOutcome::Continue
        }
        Ok(val) => {
            // Extract the (possibly mutated) response object from scope.
            let resp: ScriptResponse = scope
                .get_value::<ScriptResponse>("response")
                .unwrap_or_default();

            if val.is_bool() && !val.cast::<bool>() {
                ScriptOutcome::Abort {
                    status: resp.status.clamp(100, 999) as u16,
                    body: resp.body,
                    extra_headers: resp.extra_headers,
                }
            } else {
                ScriptOutcome::Continue
            }
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Return the compiled AST for `path`, compiling it on first access.
fn get_or_compile(path: &str) -> Result<AST, Box<rhai::EvalAltResult>> {
    let cache = ast_cache();
    if let Some(ast) = cache.get(path) {
        return Ok(ast.clone());
    }
    // Compile and insert.  If two threads race here the second write wins
    // (idempotent — both produce the same AST for the same file).
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return Err(Box::new(rhai::EvalAltResult::ErrorSystem(
                "I/O".into(),
                Box::new(e),
            )))
        }
    };
    let ast = engine().compile(&source)?;
    cache.insert(path.to_owned(), ast.clone());
    Ok(ast)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
            .collect()
    }

    fn run(script: &str, hdrs: HashMap<String, String>) -> ScriptOutcome {
        // Write script to a temp file then call run_script.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("test.rhai");
        std::fs::write(&p, script).unwrap();
        let path = p.to_str().unwrap().to_owned();
        // Clear the cache entry so this test always re-compiles.
        ast_cache().remove(&path);
        run_script(&path, "/test", "GET", "", hdrs)
    }

    #[test]
    fn script_returning_true_continues() {
        assert!(matches!(run("true", headers(&[])), ScriptOutcome::Continue));
    }

    #[test]
    fn script_returning_false_aborts() {
        assert!(matches!(
            run("false", headers(&[])),
            ScriptOutcome::Abort { .. }
        ));
    }

    #[test]
    fn script_sets_status_on_abort() {
        let script = "response.status = 401; false";
        match run(script, headers(&[])) {
            ScriptOutcome::Abort { status, .. } => assert_eq!(status, 401),
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn script_sets_body_on_abort() {
        let script = r#"response.body = "Forbidden"; false"#;
        match run(script, headers(&[])) {
            ScriptOutcome::Abort { body, .. } => assert_eq!(body, "Forbidden"),
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn script_sets_response_header_on_abort() {
        let script = r#"response.header("WWW-Authenticate", "Bearer"); false"#;
        match run(script, headers(&[])) {
            ScriptOutcome::Abort { extra_headers, .. } => {
                assert!(extra_headers
                    .iter()
                    .any(|(k, v)| k == "WWW-Authenticate" && v == "Bearer"));
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn script_reads_request_header() {
        let script = r#"
            let t = request.header("Authorization");
            if t == "" { response.status = 401; return false; }
            true
        "#;
        // Without header → abort 401.
        assert!(matches!(
            run(script, headers(&[])),
            ScriptOutcome::Abort { status: 401, .. }
        ));
        // With header → continue.
        assert!(matches!(
            run(script, headers(&[("authorization", "Bearer tok")])),
            ScriptOutcome::Continue
        ));
    }

    #[test]
    fn script_reads_request_path() {
        let script = r#"
            if request.path == "/secret" { return false; }
            true
        "#;
        assert!(matches!(run(script, headers(&[])), ScriptOutcome::Continue));
    }

    #[test]
    fn broken_script_continues_gracefully() {
        // A script that won't compile should not panic.
        assert!(matches!(
            run("let x = ;", headers(&[])),
            ScriptOutcome::Continue
        ));
    }

    #[test]
    fn script_without_explicit_return_truthy_continues() {
        // Last expression is truthy integer — treat as continue.
        assert!(matches!(run("42", headers(&[])), ScriptOutcome::Continue));
    }
}
