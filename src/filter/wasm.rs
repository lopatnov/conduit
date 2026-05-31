//! WASM plugin middleware (feature = "wasm").
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

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use dashmap::DashMap;
use wasmtime::{Caller, Engine, Linker, Module, Store};

// ── Singletons ────────────────────────────────────────────────────────────────

static WASM_ENGINE: OnceLock<Engine> = OnceLock::new();

fn engine() -> &'static Engine {
    WASM_ENGINE.get_or_init(Engine::default)
}

static WASM_MODULES: OnceLock<DashMap<String, Arc<Module>>> = OnceLock::new();

fn module_cache() -> &'static DashMap<String, Arc<Module>> {
    WASM_MODULES.get_or_init(DashMap::new)
}

pub(crate) fn get_or_compile(path: &str) -> anyhow::Result<Arc<Module>> {
    let cache = module_cache();
    if let Some(m) = cache.get(path) {
        return Ok(m.clone());
    }
    let bytes = std::fs::read(path)?;
    let module = Module::new(engine(), &bytes)?;
    let arc = Arc::new(module);
    cache.insert(path.to_owned(), arc.clone());
    Ok(arc)
}

// ── Per-call state ────────────────────────────────────────────────────────────

/// Request data the plugin can read via host functions.
#[derive(Clone)]
pub struct WasmRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub client_ip: String,
    /// Lower-cased request headers.
    pub headers: HashMap<String, String>,
    /// Header names in insertion order (for `conduit_get_header_names`).
    pub header_names: Vec<String>,
    /// X-Request-ID value (may be empty when not set by XRequestIdGuard yet).
    pub request_id: String,
    /// JSON bytes from `MiddlewareEntry.config` (empty when not configured).
    pub plugin_config: Vec<u8>,
}

/// Mutable state threaded through the Wasmtime `Store`.
struct WasmState {
    request: WasmRequest,
    response_status: u32,
    response_headers: Vec<(String, String)>,
    response_body: Vec<u8>,
    added_headers: Vec<(String, String)>,
    removed_headers: Vec<String>,
}

impl WasmState {
    fn new(request: WasmRequest) -> Self {
        Self {
            request,
            response_status: 500,
            response_headers: Vec::new(),
            response_body: Vec::new(),
            added_headers: Vec::new(),
            removed_headers: Vec::new(),
        }
    }
}

// ── Memory helpers ────────────────────────────────────────────────────────────

/// Read UTF-8 from WASM linear memory at (`ptr`, `len`).
fn mem_read_str(caller: &mut Caller<'_, WasmState>, ptr: i32, len: i32) -> String {
    let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
        return String::new();
    };
    let data = mem.data(&*caller);
    let start = ptr as usize;
    let end = start.saturating_add(len as usize);
    String::from_utf8_lossy(data.get(start..end).unwrap_or_default()).into_owned()
}

/// Write `src` into WASM memory at (`buf`, `buf_len`). Returns bytes written.
fn mem_write(caller: &mut Caller<'_, WasmState>, src: &[u8], buf: i32, buf_len: i32) -> i32 {
    let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
        return 0;
    };
    let to_write = src.len().min(buf_len as usize);
    let _ = mem.write(caller, buf as usize, &src[..to_write]);
    to_write as i32
}

// ── Outcome ───────────────────────────────────────────────────────────────────

/// What a WASM plugin returns after inspecting a request.
pub enum WasmOutcome {
    Continue {
        added_headers: Vec<(String, String)>,
        removed_headers: Vec<String>,
    },
    Abort {
        status: u16,
        body: Bytes,
        headers: Vec<(String, String)>,
    },
}

// ── Host function registration ────────────────────────────────────────────────

fn register_host_functions(linker: &mut Linker<WasmState>) -> anyhow::Result<()> {
    // conduit_get_method
    linker.func_wrap(
        "conduit",
        "conduit_get_method",
        |mut c: Caller<'_, WasmState>, buf: i32, buf_len: i32| -> i32 {
            let s = c.data().request.method.clone();
            mem_write(&mut c, s.as_bytes(), buf, buf_len)
        },
    )?;

    // conduit_get_path
    linker.func_wrap(
        "conduit",
        "conduit_get_path",
        |mut c: Caller<'_, WasmState>, buf: i32, buf_len: i32| -> i32 {
            let s = c.data().request.path.clone();
            mem_write(&mut c, s.as_bytes(), buf, buf_len)
        },
    )?;

    // conduit_get_query
    linker.func_wrap(
        "conduit",
        "conduit_get_query",
        |mut c: Caller<'_, WasmState>, buf: i32, buf_len: i32| -> i32 {
            let s = c.data().request.query.clone();
            mem_write(&mut c, s.as_bytes(), buf, buf_len)
        },
    )?;

    // conduit_get_client_ip
    linker.func_wrap(
        "conduit",
        "conduit_get_client_ip",
        |mut c: Caller<'_, WasmState>, buf: i32, buf_len: i32| -> i32 {
            let s = c.data().request.client_ip.clone();
            mem_write(&mut c, s.as_bytes(), buf, buf_len)
        },
    )?;

    // conduit_get_header
    linker.func_wrap(
        "conduit",
        "conduit_get_header",
        |mut c: Caller<'_, WasmState>,
         name_ptr: i32,
         name_len: i32,
         buf: i32,
         buf_len: i32|
         -> i32 {
            let name = mem_read_str(&mut c, name_ptr, name_len).to_ascii_lowercase();
            let value = c.data().request.headers.get(&name).cloned();
            match value {
                Some(v) => mem_write(&mut c, v.as_bytes(), buf, buf_len),
                None => -1,
            }
        },
    )?;

    // conduit_set_request_header
    linker.func_wrap(
        "conduit",
        "conduit_set_request_header",
        |mut c: Caller<'_, WasmState>, name_ptr: i32, name_len: i32, val_ptr: i32, val_len: i32| {
            let name = mem_read_str(&mut c, name_ptr, name_len);
            let val = mem_read_str(&mut c, val_ptr, val_len);
            c.data_mut().added_headers.push((name, val));
        },
    )?;

    // conduit_remove_request_header
    linker.func_wrap(
        "conduit",
        "conduit_remove_request_header",
        |mut c: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| {
            let name = mem_read_str(&mut c, name_ptr, name_len);
            c.data_mut().removed_headers.push(name);
        },
    )?;

    // conduit_set_response_status
    linker.func_wrap(
        "conduit",
        "conduit_set_response_status",
        |mut c: Caller<'_, WasmState>, status: i32| {
            c.data_mut().response_status = (status.max(100).min(999)) as u32;
        },
    )?;

    // conduit_set_response_header
    linker.func_wrap(
        "conduit",
        "conduit_set_response_header",
        |mut c: Caller<'_, WasmState>, name_ptr: i32, name_len: i32, val_ptr: i32, val_len: i32| {
            let name = mem_read_str(&mut c, name_ptr, name_len);
            let val = mem_read_str(&mut c, val_ptr, val_len);
            c.data_mut().response_headers.push((name, val));
        },
    )?;

    // conduit_set_response_body
    linker.func_wrap(
        "conduit",
        "conduit_set_response_body",
        |mut c: Caller<'_, WasmState>, body_ptr: i32, body_len: i32| {
            let Some(mem) = c.get_export("memory").and_then(|e| e.into_memory()) else {
                return;
            };
            let start = body_ptr as usize;
            let end = start.saturating_add(body_len as usize);
            let bytes = mem.data(&c).get(start..end).unwrap_or_default().to_vec();
            c.data_mut().response_body = bytes;
        },
    )?;

    // conduit_get_plugin_config
    linker.func_wrap(
        "conduit",
        "conduit_get_plugin_config",
        |mut c: Caller<'_, WasmState>, buf: i32, buf_len: i32| -> i32 {
            let cfg = c.data().request.plugin_config.clone();
            mem_write(&mut c, &cfg, buf, buf_len)
        },
    )?;

    // conduit_log
    linker.func_wrap(
        "conduit",
        "conduit_log",
        |mut c: Caller<'_, WasmState>, level: i32, msg_ptr: i32, msg_len: i32| {
            let msg = mem_read_str(&mut c, msg_ptr, msg_len);
            match level {
                0 => tracing::trace!(wasm = true, "{msg}"),
                1 => tracing::debug!(wasm = true, "{msg}"),
                2 => tracing::info!(wasm = true, "{msg}"),
                3 => tracing::warn!(wasm = true, "{msg}"),
                _ => tracing::error!(wasm = true, "{msg}"),
            }
        },
    )?;

    // ── New in V2: additional read + control functions ─────────────────────────

    // conduit_get_uri — full URI (path + optional "?query")
    linker.func_wrap(
        "conduit",
        "conduit_get_uri",
        |mut c: Caller<'_, WasmState>, buf: i32, buf_len: i32| -> i32 {
            let uri = {
                let req = &c.data().request;
                if req.query.is_empty() {
                    req.path.clone()
                } else {
                    format!("{}?{}", req.path, req.query)
                }
            };
            mem_write(&mut c, uri.as_bytes(), buf, buf_len)
        },
    )?;

    // conduit_get_request_id — X-Request-ID value (empty string if absent)
    linker.func_wrap(
        "conduit",
        "conduit_get_request_id",
        |mut c: Caller<'_, WasmState>, buf: i32, buf_len: i32| -> i32 {
            let id = c.data().request.request_id.clone();
            mem_write(&mut c, id.as_bytes(), buf, buf_len)
        },
    )?;

    // conduit_get_header_count — number of request headers
    linker.func_wrap(
        "conduit",
        "conduit_get_header_count",
        |c: Caller<'_, WasmState>| -> i32 { c.data().request.headers.len() as i32 },
    )?;

    // conduit_get_header_names — all header names as newline-separated UTF-8.
    // Order matches insertion order recorded in WasmRequest.header_names.
    // Returns total bytes written (may be truncated at buf_len).
    linker.func_wrap(
        "conduit",
        "conduit_get_header_names",
        |mut c: Caller<'_, WasmState>, buf: i32, buf_len: i32| -> i32 {
            let names = c.data().request.header_names.join("\n");
            mem_write(&mut c, names.as_bytes(), buf, buf_len)
        },
    )?;

    // conduit_abort_with_redirect — convenience: abort with 302 + Location header.
    // The plugin must still call on_request() → 1 to trigger the abort path.
    linker.func_wrap(
        "conduit",
        "conduit_abort_with_redirect",
        |mut c: Caller<'_, WasmState>, url_ptr: i32, url_len: i32| {
            let url = mem_read_str(&mut c, url_ptr, url_len);
            let state = c.data_mut();
            state.response_status = 302;
            state.response_headers.push(("location".to_owned(), url));
            state.response_body = b"Redirecting...".to_vec();
        },
    )?;

    Ok(())
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the WASM plugin at `path` against the given request.
///
/// Fail-open: any error (missing file, compile, link, trap) logs a warning
/// and returns `Continue` — same behaviour as Rhai scripting.
pub fn run_wasm(request: WasmRequest, path: &str) -> WasmOutcome {
    match run_inner(request, path) {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::warn!(
                plugin = path,
                error = %e,
                "WASM plugin error — request passes through (fail-open)"
            );
            WasmOutcome::Continue {
                added_headers: Vec::new(),
                removed_headers: Vec::new(),
            }
        }
    }
}

fn run_inner(request: WasmRequest, path: &str) -> anyhow::Result<WasmOutcome> {
    let module = get_or_compile(path)?;
    let mut store = Store::new(engine(), WasmState::new(request));
    let mut linker = Linker::new(engine());
    register_host_functions(&mut linker)?;

    let instance = linker.instantiate(&mut store, &module)?;
    let on_request = instance
        .get_typed_func::<(), i32>(&mut store, "on_request")
        .map_err(|e| anyhow::anyhow!("WASM module missing 'on_request' export: {e}"))?;

    let ret = on_request.call(&mut store, ())?;
    let state = store.into_data();

    if ret == 0 {
        Ok(WasmOutcome::Continue {
            added_headers: state.added_headers,
            removed_headers: state.removed_headers,
        })
    } else {
        Ok(WasmOutcome::Abort {
            status: state.response_status.clamp(100, 999) as u16,
            body: Bytes::from(state.response_body),
            headers: state.response_headers,
        })
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn req() -> WasmRequest {
        WasmRequest {
            method: "GET".into(),
            path: "/".into(),
            query: String::new(),
            client_ip: "127.0.0.1".into(),
            headers: HashMap::new(),
            header_names: Vec::new(),
            request_id: String::new(),
            plugin_config: Vec::new(),
        }
    }

    fn compile_wat(src: &str) -> (tempfile::NamedTempFile, String) {
        let wasm = wat::parse_str(src).expect("WAT parse");
        let mut f = tempfile::Builder::new()
            .suffix(".wasm")
            .tempfile()
            .expect("tempfile");
        f.write_all(&wasm).expect("write");
        f.flush().expect("flush");
        let path = f.path().to_string_lossy().into_owned();
        (f, path)
    }

    // ── Passthrough / abort ───────────────────────────────────────────────────

    #[test]
    fn passthrough_returns_continue() {
        let (_f, p) =
            compile_wat("(module (func (export \"on_request\") (result i32) i32.const 0))");
        assert!(matches!(run_wasm(req(), &p), WasmOutcome::Continue { .. }));
    }

    #[test]
    fn abort_returns_status_403() {
        let (_f, p) = compile_wat(
            r#"(module
          (import "conduit" "conduit_set_response_status" (func $s (param i32)))
          (func (export "on_request") (result i32)
            i32.const 403  call $s  i32.const 1))"#,
        );
        match run_wasm(req(), &p) {
            WasmOutcome::Abort { status, .. } => assert_eq!(status, 403),
            _ => panic!("expected Abort"),
        }
    }

    #[test]
    fn abort_without_set_status_defaults_to_500() {
        let (_f, p) =
            compile_wat("(module (func (export \"on_request\") (result i32) i32.const 1))");
        match run_wasm(req(), &p) {
            WasmOutcome::Abort { status, .. } => assert_eq!(status, 500),
            _ => panic!("expected Abort"),
        }
    }

    // ── Fail-open ─────────────────────────────────────────────────────────────

    #[test]
    fn missing_file_fails_open() {
        assert!(matches!(
            run_wasm(req(), "/nonexistent/plugin.wasm"),
            WasmOutcome::Continue { .. }
        ));
    }

    #[test]
    fn invalid_wasm_bytes_fails_open() {
        let mut f = tempfile::Builder::new()
            .suffix(".wasm")
            .tempfile()
            .expect("tempfile");
        f.write_all(b"NOT VALID WASM").expect("write");
        f.flush().expect("flush");
        assert!(matches!(
            run_wasm(req(), &f.path().to_string_lossy()),
            WasmOutcome::Continue { .. }
        ));
    }

    #[test]
    fn missing_on_request_export_fails_open() {
        let (_f, p) = compile_wat("(module (func (export \"wrong\") (result i32) i32.const 0))");
        assert!(matches!(run_wasm(req(), &p), WasmOutcome::Continue { .. }));
    }

    // ── Module caching ────────────────────────────────────────────────────────

    #[test]
    fn same_path_returns_cached_arc() {
        let (_f, p) =
            compile_wat("(module (func (export \"on_request\") (result i32) i32.const 0))");
        module_cache().remove(&p);
        let m1 = get_or_compile(&p).unwrap();
        let m2 = get_or_compile(&p).unwrap();
        assert!(Arc::ptr_eq(&m1, &m2));
    }

    // ── Header read ───────────────────────────────────────────────────────────

    #[test]
    fn plugin_reads_missing_header_gets_minus_one() {
        // Plugin: read "x-api-key", abort with 401 if absent (-1), else 200.
        let (_f, p) = compile_wat(
            r#"(module
          (import "conduit" "conduit_get_header"          (func $gh (param i32 i32 i32 i32) (result i32)))
          (import "conduit" "conduit_set_response_status" (func $ss (param i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "x-api-key")
          (func (export "on_request") (result i32)
            i32.const 0 i32.const 9 i32.const 32 i32.const 64 call $gh
            i32.const -1 i32.eq
            if  i32.const 401 call $ss  i32.const 1  return  end
            i32.const 200 call $ss  i32.const 1))"#,
        );

        // Absent → 401.
        match run_wasm(req(), &p) {
            WasmOutcome::Abort { status, .. } => assert_eq!(status, 401),
            _ => panic!(),
        }

        // Present → 200.
        let mut r = req();
        r.headers.insert("x-api-key".into(), "tok".into());
        match run_wasm(r, &p) {
            WasmOutcome::Abort { status, .. } => assert_eq!(status, 200),
            _ => panic!(),
        }
    }

    // ── Header injection ──────────────────────────────────────────────────────

    #[test]
    fn plugin_adds_request_header() {
        let (_f, p) = compile_wat(
            r#"(module
          (import "conduit" "conduit_set_request_header" (func $srh (param i32 i32 i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0)  "x-wasm-plugin")
          (data (i32.const 16) "active")
          (func (export "on_request") (result i32)
            i32.const 0  i32.const 13
            i32.const 16 i32.const 6
            call $srh
            i32.const 0))"#,
        );
        match run_wasm(req(), &p) {
            WasmOutcome::Continue { added_headers, .. } => {
                assert_eq!(added_headers.len(), 1);
                assert_eq!(added_headers[0], ("x-wasm-plugin".into(), "active".into()));
            }
            _ => panic!(),
        }
    }

    // ── Plugin config ─────────────────────────────────────────────────────────

    #[test]
    fn plugin_config_bytes_forwarded() {
        // Plugin reads config: if any bytes received → abort 202; else abort 400.
        // This verifies config is forwarded without relying on the byte count as status.
        let (_f, p) = compile_wat(
            r#"(module
          (import "conduit" "conduit_get_plugin_config"   (func $cfg (param i32 i32) (result i32)))
          (import "conduit" "conduit_set_response_status" (func $ss  (param i32)))
          (memory (export "memory") 1)
          (func (export "on_request") (result i32)
            i32.const 0 i32.const 256 call $cfg  ;; returns bytes written
            i32.const 0
            i32.gt_s
            if  i32.const 202 call $ss  else  i32.const 400 call $ss  end
            i32.const 1))"#,
        );

        // With non-empty config → 202.
        let mut r = req();
        r.plugin_config = b"{\"key\":\"value\"}".to_vec();
        match run_wasm(r, &p) {
            WasmOutcome::Abort { status, .. } => assert_eq!(status, 202),
            _ => panic!("expected Abort"),
        }

        // Without config → 400.
        match run_wasm(req(), &p) {
            WasmOutcome::Abort { status, .. } => assert_eq!(status, 400),
            _ => panic!("expected Abort"),
        }
    }

    // ── V2: new host functions ────────────────────────────────────────────────

    #[test]
    fn get_uri_path_only() {
        // Plugin reads the full URI via conduit_get_uri and echos it back in X-Path.
        // WAT: allocate a fixed buffer, call conduit_get_uri, set the result as a
        // request header, then continue.
        const WAT: &str = r#"(module
            (import "conduit" "conduit_get_uri" (func $uri (param i32 i32) (result i32)))
            (import "conduit" "conduit_set_request_header"
                (func $srhdr (param i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "x-uri")
            (func (export "on_request") (result i32)
                (local $n i32)
                ;; get_uri into [64, 256)
                (local.set $n (call $uri (i32.const 64) (i32.const 256)))
                ;; set_request_header "x-uri" = result
                (call $srhdr
                    (i32.const 0) (i32.const 5)    ;; name = "x-uri"
                    (i32.const 64) (local.get $n))  ;; value = uri bytes
                i32.const 0)
        )"#;
        let mut r = req();
        r.path = "/api/v1".into();
        r.query = String::new();
        let (_f, path) = compile_wat(WAT);
        let outcome = run_wasm(r, &path);
        match outcome {
            WasmOutcome::Continue { added_headers, .. } => {
                let uri_hdr = added_headers.iter().find(|(k, _)| k == "x-uri");
                assert_eq!(uri_hdr.map(|(_, v)| v.as_str()), Some("/api/v1"));
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn get_uri_with_query() {
        const WAT: &str = r#"(module
            (import "conduit" "conduit_get_uri" (func $uri (param i32 i32) (result i32)))
            (import "conduit" "conduit_set_request_header"
                (func $srhdr (param i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "x-uri")
            (func (export "on_request") (result i32)
                (local $n i32)
                (local.set $n (call $uri (i32.const 64) (i32.const 256)))
                (call $srhdr (i32.const 0) (i32.const 5) (i32.const 64) (local.get $n))
                i32.const 0)
        )"#;
        let mut r = req();
        r.path = "/search".into();
        r.query = "q=hello&lang=en".into();
        let (_f, path) = compile_wat(WAT);
        let outcome = run_wasm(r, &path);
        match outcome {
            WasmOutcome::Continue { added_headers, .. } => {
                let uri_hdr = added_headers.iter().find(|(k, _)| k == "x-uri");
                assert_eq!(
                    uri_hdr.map(|(_, v)| v.as_str()),
                    Some("/search?q=hello&lang=en")
                );
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn get_header_count_returns_correct_number() {
        // Plugin stores the header count (as ASCII digit) into a request header.
        // 3 headers → stores "3" in x-count header, then continues.
        const WAT: &str = r#"(module
            (import "conduit" "conduit_get_header_count" (func $cnt (result i32)))
            (import "conduit" "conduit_set_request_header"
                (func $srhdr (param i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "x-count")   ;; name at 0, len 7
            ;; buf for the ascii digit at offset 32
            (func (export "on_request") (result i32)
                (local $n i32)
                (local.set $n (call $cnt))
                ;; write ASCII digit (count + '0' = count + 48) into buf[32]
                (i32.store8 (i32.const 32)
                    (i32.add (local.get $n) (i32.const 48)))
                (call $srhdr
                    (i32.const 0) (i32.const 7)   ;; "x-count"
                    (i32.const 32) (i32.const 1))  ;; single ASCII digit
                i32.const 0)
        )"#;
        let mut r = req();
        r.headers
            .insert("content-type".into(), "application/json".into());
        r.headers
            .insert("authorization".into(), "Bearer token".into());
        r.headers.insert("x-custom".into(), "value".into());
        let (_f, path) = compile_wat(WAT);
        let outcome = run_wasm(r, &path);
        match outcome {
            WasmOutcome::Continue { added_headers, .. } => {
                let hdr = added_headers.iter().find(|(k, _)| k == "x-count");
                assert_eq!(
                    hdr.map(|(_, v)| v.as_str()),
                    Some("3"),
                    "header count should be 3"
                );
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn get_request_id_returns_value() {
        const WAT: &str = r#"(module
            (import "conduit" "conduit_get_request_id"
                (func $rid (param i32 i32) (result i32)))
            (import "conduit" "conduit_set_request_header"
                (func $srhdr (param i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "x-got-id")
            (func (export "on_request") (result i32)
                (local $n i32)
                (local.set $n (call $rid (i32.const 64) (i32.const 256)))
                (call $srhdr (i32.const 0) (i32.const 8) (i32.const 64) (local.get $n))
                i32.const 0)
        )"#;
        let mut r = req();
        r.request_id = "test-uuid-1234".into();
        let (_f, path) = compile_wat(WAT);
        let outcome = run_wasm(r, &path);
        match outcome {
            WasmOutcome::Continue { added_headers, .. } => {
                let hdr = added_headers.iter().find(|(k, _)| k == "x-got-id");
                assert_eq!(hdr.map(|(_, v)| v.as_str()), Some("test-uuid-1234"));
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn abort_with_redirect_sets_location() {
        const WAT: &str = r#"(module
            (import "conduit" "conduit_abort_with_redirect"
                (func $redir (param i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "https://example.com/new")
            (func (export "on_request") (result i32)
                (call $redir (i32.const 0) (i32.const 23))
                i32.const 1)
        )"#;
        let (_f, path) = compile_wat(WAT);
        let outcome = run_wasm(req(), &path);
        match outcome {
            WasmOutcome::Abort {
                status, headers, ..
            } => {
                assert_eq!(status, 302, "redirect must be 302");
                let loc = headers.iter().find(|(k, _)| k == "location");
                assert_eq!(
                    loc.map(|(_, v)| v.as_str()),
                    Some("https://example.com/new"),
                    "Location header must be set"
                );
            }
            _ => panic!("expected Abort"),
        }
    }
}
