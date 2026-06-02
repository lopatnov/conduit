# WASM Plugin SDK for Conduit

> **Requires** `cargo build --features wasm`

Write middleware plugins in any language that compiles to WebAssembly.
Plugins run in an isolated Wasmtime sandbox with CPU fuel limits and fail-open
semantics — a crashing plugin never takes down the server.

## Quick start

```yaml
# conduit.yaml
middleware:
  - type: wasm
    path: ./my-plugin.wasm
    config: { "apiKey": "secret" }   # optional — read with conduit_get_plugin_config
```

## Plugin contract

A plugin is a `.wasm` module that:

1. **Must export `on_request() → i32`** — called on every request before forwarding
   - Return `0` → continue to upstream
   - Return `1` → abort (use `conduit_set_response_*` to set the error response)
2. **May export `on_response(status: i32) → i32`** — called after the upstream responds
   - `status` is the upstream HTTP status code
   - Return value is **ignored** — any headers/body set via response host functions are always applied
   - Modifications via `conduit_set_response_header`, `conduit_remove_response_header`,
     and `conduit_set_response_body` take effect regardless of the return value
3. **Must export `"memory"`** — Conduit reads/writes plugin memory via this export

**Fail-open:** compile errors, link errors, missing exports, and runtime traps all
let the request pass through unchanged. The error is logged at `WARN` level.

---

## ABI reference

All functions are imported from the `"conduit"` module.

### Request — read

| Function | Signature | Description |
|---|---|---|
| `conduit_get_method` | `(buf, buf_len) → i32` | HTTP method (`GET`, `POST`, …) |
| `conduit_get_path` | `(buf, buf_len) → i32` | Request path without query string |
| `conduit_get_query` | `(buf, buf_len) → i32` | Raw query string (empty if none) |
| `conduit_get_uri` | `(buf, buf_len) → i32` | Full URI: path + `?` + query |
| `conduit_get_client_ip` | `(buf, buf_len) → i32` | Remote IP address |
| `conduit_get_request_id` | `(buf, buf_len) → i32` | `X-Request-ID` header value |
| `conduit_get_header` | `(name, nlen, buf, buf_len) → i32` | Named request header value; `-1` if absent |
| `conduit_get_header_count` | `() → i32` | Number of request headers |
| `conduit_get_header_names` | `(buf, buf_len) → i32` | Newline-separated header names |
| `conduit_get_plugin_config` | `(buf, buf_len) → i32` | JSON string from `MiddlewareEntry.config` |

### Request — mutate

| Function | Signature | Description |
|---|---|---|
| `conduit_set_request_header` | `(name, nlen, val, vlen)` | Add or overwrite a request header before forwarding to upstream |
| `conduit_remove_request_header` | `(name, nlen)` | Remove a request header |

### Abort response (used in `on_request` when returning 1)

| Function | Signature | Description |
|---|---|---|
| `conduit_set_response_status` | `(status: i32)` | HTTP status code for the abort response |
| `conduit_set_response_header` | `(name, nlen, val, vlen)` | Add header to the abort response |
| `conduit_set_response_body` | `(body, body_len)` | Set body of the abort response |
| `conduit_abort_with_redirect` | `(url, url_len)` | Abort with `302 Found` + `Location` header |

### Response — read (available in `on_response`)

| Function | Signature | Description |
|---|---|---|
| `conduit_get_response_status` | `() → i32` | Upstream HTTP status code |
| `conduit_get_response_header` | `(name, nlen, buf, buf_len) → i32` | Named upstream response header; `-1` if absent |

### Response — mutate (available in `on_response`)

| Function | Signature | Description |
|---|---|---|
| `conduit_set_response_header` | `(name, nlen, val, vlen)` | Add or overwrite an upstream response header |
| `conduit_remove_response_header` | `(name, nlen)` | Remove an upstream response header |
| `conduit_set_response_body` | `(body, body_len)` | Replace upstream response body |

### Logging

| Function | Signature | Description |
|---|---|---|
| `conduit_log` | `(level: i32, msg, msg_len)` | Emit a log entry: `0`=trace `1`=debug `2`=info `3`=warn `4`=error |

**Buffer convention:** all `(buf, buf_len) → i32` read functions write UTF-8 bytes
into the plugin's memory at `buf` up to `buf_len` bytes and return the number of bytes
written (or `-1` for "absent"). Pass `null_ptr / 0` to query the required size without
writing anything.

---

## Building a plugin

### From WAT (WebAssembly Text)

```bash
# Using wasm-tools
cargo install wasm-tools
wasm-tools parse my-plugin.wat -o my-plugin.wasm

# Using wabt
wat2wasm my-plugin.wat -o my-plugin.wasm
```

### From Rust

```toml
# Cargo.toml
[lib]
crate-type = ["cdylib"]

[profile.release]
opt-level = "z"
strip = true
```

```rust
// src/lib.rs  — target: wasm32-wasip1 or wasm32-unknown-unknown

unsafe extern "C" {
    fn conduit_get_header(name: *const u8, name_len: i32, buf: *mut u8, buf_len: i32) -> i32;
    fn conduit_set_response_status(status: i32);
    fn conduit_set_response_body(body: *const u8, body_len: i32);
}

#[unsafe(no_mangle)]
pub extern "C" fn on_request() -> i32 {
    let key = b"x-api-key";
    let mut buf = [0u8; 256];
    let n = unsafe {
        conduit_get_header(key.as_ptr(), key.len() as i32, buf.as_mut_ptr(), 256)
    };
    if n < 0 {
        let msg = b"{\"error\":\"missing api key\"}";
        unsafe {
            conduit_set_response_status(401);
            conduit_set_response_body(msg.as_ptr(), msg.len() as i32);
        }
        return 1; // abort
    }
    0 // continue
}
```

```bash
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/my_plugin.wasm .
```

### From C

```c
// plugin.c
extern int  conduit_get_header(const char*, int, char*, int);
extern void conduit_set_response_status(int);
extern void conduit_set_response_body(const char*, int);

__attribute__((export_name("on_request")))
int on_request(void) {
    char buf[256];
    int n = conduit_get_header("x-api-key", 9, buf, sizeof(buf));
    if (n < 0) {
        conduit_set_response_status(401);
        const char *msg = "missing api key";
        conduit_set_response_body(msg, 15);
        return 1;
    }
    return 0;
}
```

```bash
clang --target=wasm32-unknown-unknown -nostdlib \
  -Wl,--export=on_request -Wl,--export=memory \
  -Wl,--no-entry -o plugin.wasm plugin.c
```

---

## Response-phase example

```rust
unsafe extern "C" {
    fn conduit_get_response_status() -> i32;
    fn conduit_set_response_header(name: *const u8, nlen: i32, val: *const u8, vlen: i32);
}

#[unsafe(no_mangle)]
pub extern "C" fn on_response(status: i32) -> i32 {
    // Tag every 5xx response with a custom header
    if status >= 500 {
        let name = b"x-error-source";
        let val  = b"upstream";
        unsafe {
            conduit_set_response_header(
                name.as_ptr(), name.len() as i32,
                val.as_ptr(),  val.len() as i32,
            );
        }
    }
    0 // continue
}

// on_request is still required
#[unsafe(no_mangle)]
pub extern "C" fn on_request() -> i32 { 0 }
```

---

## Full SDK reference

See `sdk/conduit.wat` for the canonical WAT import declarations for all 17 host functions.

## Examples

- `example-auth/plugin.wat` — API key check with 302 redirect on failure
- See also: `examples/middleware-demo/` — full Rhai + WASM pipeline demo
- See also: [docs/wasm.md](../../docs/wasm.md) — Rust, C, and Go examples with build guide
