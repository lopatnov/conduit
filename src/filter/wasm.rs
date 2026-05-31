//! WASM plugin middleware (feature = "wasm").
//!
//! Plugins are compiled `.wasm` binaries that export a single `on_request`
//! function.  The host exposes a small set of `conduit_*` functions for
//! reading the request and writing a rejection response.
//!
//! ## Plugin ABI
//!
//! **Imports** (namespace `"conduit"`):
//!
//! | Function | Description |
//! |---|---|
//! | `conduit_get_method(buf, buf_len) -> i32` | Write HTTP method; returns bytes written |
//! | `conduit_get_path(buf, buf_len) -> i32` | Request path |
//! | `conduit_get_query(buf, buf_len) -> i32` | Query string (empty if none) |
//! | `conduit_get_client_ip(buf, buf_len) -> i32` | Remote IP |
//! | `conduit_get_header(name, nlen, buf, buf_len) -> i32` | Header value; -1 if absent |
//! | `conduit_set_request_header(name, nlen, val, vlen)` | Add/overwrite request header |
//! | `conduit_remove_request_header(name, nlen)` | Remove a request header |
//! | `conduit_set_response_status(status)` | Abort response status |
//! | `conduit_set_response_header(name, nlen, val, vlen)` | Abort response header |
//! | `conduit_set_response_body(body, body_len)` | Abort response body |
//! | `conduit_get_plugin_config(buf, buf_len) -> i32` | JSON from `MiddlewareEntry.config` |
//! | `conduit_log(level, msg, msg_len)` | 0=trace 1=debug 2=info 3=warn 4=error |
//!
//! **Export** (required):
//! ```text
//! on_request() -> i32    // 0 = Continue, 1 = Abort
//! ```
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
}
