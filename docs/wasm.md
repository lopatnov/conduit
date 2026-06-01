# WASM Middleware

Conduit can load WebAssembly plugins as middleware. Plugins run for every
request and can inspect or modify headers, short-circuit with a custom
response, or redirect the client.

> **Requires** `cargo build --features wasm`

---

## Table of Contents

- [Configuration](#configuration)
- [Plugin ABI](#plugin-abi)
  - [Required exports](#required-exports)
  - [Host functions — read request](#host-functions--read-request)
  - [Host functions — mutate request](#host-functions--mutate-request)
  - [Host functions — abort response](#host-functions--abort-response)
  - [Host functions — logging](#host-functions--logging)
- [Memory conventions](#memory-conventions)
- [Examples](#examples)
  - [Minimal plugin (WAT)](#minimal-plugin-wat)
  - [Header check in Rust](#header-check-in-rust)
  - [Inject a header in Rust](#inject-a-header-in-rust)
  - [Redirect old paths in Rust](#redirect-old-paths-in-rust)
  - [Using plugin config in Rust](#using-plugin-config-in-rust)
- [Building a Rust plugin](#building-a-rust-plugin)
- [Execution model](#execution-model)
- [Error handling](#error-handling)
- [Comparison with Rhai](#comparison-with-rhai)

---

## Configuration

```yaml
middleware:
  - type: wasm
    path: ./plugins/auth-check.wasm

  # Multiple plugins run in order — first abort wins
  - type: wasm
    path: ./plugins/rate-check.wasm

  # Pass a JSON config object to the plugin
  - type: wasm
    path: ./plugins/validator.wasm
    config:
      max_size_kb: 512
      allowed_origins: ["https://app.example.com"]
```

```json
{
  "middleware": [
    { "type": "wasm", "path": "./plugins/auth-check.wasm" },
    {
      "type": "wasm",
      "path": "./plugins/validator.wasm",
      "config": { "max_size_kb": 512 }
    }
  ]
}
```

Rhai and WASM entries can be mixed freely — they run in declaration order:

```yaml
middleware:
  - type: script
    path: ./scripts/ip-check.rhai
  - type: wasm
    path: ./plugins/jwt-validate.wasm
  - type: script
    path: ./scripts/log-request.rhai
```

---

## Plugin ABI

Plugins communicate with Conduit through a set of **host functions** imported
from the `"conduit"` namespace, and a single **exported function** that
Conduit calls for each request.

### Required exports

| Export | Signature | Description |
| ------ | --------- | ----------- |
| `on_request` | `() -> i32` | Called for every request. Return `0` to continue, `1` (or any non-zero) to abort |
| `memory` | linear memory | Must be exported — all string data passes through it |

### Host functions — read request

All read functions write their result into a caller-supplied buffer and return
the number of bytes written. If the buffer is too small, the result is
truncated (no error). If the value does not exist (e.g. header not found),
`-1` is returned.

| Function | Description |
| -------- | ----------- |
| `conduit_get_method(buf: i32, buf_len: i32) -> i32` | HTTP method (`"GET"`, `"POST"`, …) |
| `conduit_get_path(buf: i32, buf_len: i32) -> i32` | Request path, e.g. `"/api/users"` |
| `conduit_get_query(buf: i32, buf_len: i32) -> i32` | Raw query string; empty when absent |
| `conduit_get_uri(buf: i32, buf_len: i32) -> i32` | Full URI: path + `"?"` + query |
| `conduit_get_client_ip(buf: i32, buf_len: i32) -> i32` | Remote client IP address |
| `conduit_get_request_id(buf: i32, buf_len: i32) -> i32` | `X-Request-ID` header value |
| `conduit_get_header(name_ptr: i32, name_len: i32, buf: i32, buf_len: i32) -> i32` | Named header value; `-1` if absent. Look-up is **case-insensitive** |
| `conduit_get_header_count() -> i32` | Number of request headers |
| `conduit_get_header_names(buf: i32, buf_len: i32) -> i32` | All header names, newline-separated |
| `conduit_get_plugin_config(buf: i32, buf_len: i32) -> i32` | JSON bytes from `middleware[].config`; empty when not set |

### Host functions — mutate request

Header mutations are collected during the plugin call and applied to the
upstream request **after** `on_request` returns `0` (continue). They have no
effect if the plugin aborts.

| Function | Description |
| -------- | ----------- |
| `conduit_set_request_header(name_ptr: i32, name_len: i32, val_ptr: i32, val_len: i32)` | Add or overwrite a request header |
| `conduit_remove_request_header(name_ptr: i32, name_len: i32)` | Remove a request header |

### Host functions — abort response

Call these **before** returning `1` from `on_request`. They have no effect
when the plugin continues (`return 0`).

| Function | Description |
| -------- | ----------- |
| `conduit_set_response_status(status: i32)` | HTTP status code (clamped to 100–999; default: `500`) |
| `conduit_set_response_header(name_ptr: i32, name_len: i32, val_ptr: i32, val_len: i32)` | Add a response header |
| `conduit_set_response_body(body_ptr: i32, body_len: i32)` | Set the response body (bytes, not required to be UTF-8) |
| `conduit_abort_with_redirect(url_ptr: i32, url_len: i32)` | Shortcut: sets status 302 + `Location` header + body `"Redirecting..."`. Still return `1`. |

### Host functions — logging

| Function | Description |
| -------- | ----------- |
| `conduit_log(level: i32, msg_ptr: i32, msg_len: i32)` | Write to the Conduit log. Levels: `0`=trace `1`=debug `2`=info `3`=warn `4`=error |

---

## Memory conventions

All string data passes through the plugin's **linear memory**:

- To **read** from a host function: allocate a buffer in WASM memory, pass
  `(ptr, len)` to the function, then read up to the returned byte count.
- To **write** to a host function: write the string into WASM memory, then
  pass `(ptr, len)` to the function.
- Conduit **never retains pointers** after the host function returns — no
  dangling-pointer risk.
- The host function return value is the number of bytes written (or `-1` for
  missing headers). A return less than `buf_len` means the full value fit.

---

## Examples

### Minimal plugin (WAT)

The smallest possible plugin — always passes through:

```wat
(module
  (memory (export "memory") 1)
  (func (export "on_request") (result i32)
    i32.const 0   ;; 0 = continue
  )
)
```

Compile with `wat2wasm`:
```bash
wat2wasm minimal.wat -o minimal.wasm
```

---

### Header check in Rust

A plugin that returns `401` when `X-API-Key` is missing or wrong.

```rust
// src/lib.rs
use std::ffi::CStr;

// Import host functions.
extern "C" {
    fn conduit_get_header(
        name_ptr: i32, name_len: i32,
        buf: i32, buf_len: i32,
    ) -> i32;
    fn conduit_set_response_status(status: i32);
    fn conduit_set_response_body(body_ptr: i32, body_len: i32);
}

static mut BUF: [u8; 256] = [0u8; 256];

#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    let key_name = b"x-api-key";
    let n = unsafe {
        conduit_get_header(
            key_name.as_ptr() as i32, key_name.len() as i32,
            BUF.as_ptr() as i32, BUF.len() as i32,
        )
    };

    if n < 0 {
        // Header absent.
        reject(401, b"missing API key");
        return 1;
    }

    let value = unsafe { &BUF[..n as usize] };
    if value != b"my-secret" {
        reject(403, b"invalid API key");
        return 1;
    }

    0 // continue
}

fn reject(status: i32, msg: &[u8]) {
    unsafe {
        conduit_set_response_status(status);
        conduit_set_response_body(msg.as_ptr() as i32, msg.len() as i32);
    }
}
```

Build:
```bash
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/my_plugin.wasm ./plugins/
```

---

### Inject a header in Rust

Add `X-Plugin-Version: 1.0` to every forwarded request:

```rust
extern "C" {
    fn conduit_set_request_header(
        name_ptr: i32, name_len: i32,
        val_ptr: i32, val_len: i32,
    );
}

#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    let name = b"x-plugin-version";
    let value = b"1.0";
    unsafe {
        conduit_set_request_header(
            name.as_ptr() as i32, name.len() as i32,
            value.as_ptr() as i32, value.len() as i32,
        );
    }
    0 // continue — header is applied to upstream request
}
```

---

### Redirect old paths in Rust

Permanently redirect `/old-api/` to `/api/`:

```rust
extern "C" {
    fn conduit_get_path(buf: i32, buf_len: i32) -> i32;
    fn conduit_abort_with_redirect(url_ptr: i32, url_len: i32);
}

static mut PATH_BUF: [u8; 512] = [0u8; 512];

#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    let n = unsafe {
        conduit_get_path(PATH_BUF.as_ptr() as i32, PATH_BUF.len() as i32)
    };
    if n <= 0 {
        return 0;
    }
    let path = unsafe { &PATH_BUF[..n as usize] };
    if path.starts_with(b"/old-api/") {
        let new_path = b"/api/";
        unsafe {
            conduit_abort_with_redirect(
                new_path.as_ptr() as i32,
                new_path.len() as i32,
            );
        }
        return 1; // abort with 302
    }
    0
}
```

---

### Using plugin config in Rust

Read the `config` JSON passed from `conduit.yaml`:

```yaml
# conduit.yaml
middleware:
  - type: wasm
    path: ./plugins/validator.wasm
    config:
      allowed_key: "secret-abc"
```

```rust
extern "C" {
    fn conduit_get_plugin_config(buf: i32, buf_len: i32) -> i32;
    fn conduit_get_header(name_ptr: i32, name_len: i32, buf: i32, buf_len: i32) -> i32;
    fn conduit_set_response_status(status: i32);
}

static mut CFG_BUF: [u8; 1024] = [0u8; 1024];
static mut HDR_BUF: [u8; 256] = [0u8; 256];

#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    // Read config JSON, e.g. {"allowed_key":"secret-abc"}
    let cfg_len = unsafe {
        conduit_get_plugin_config(CFG_BUF.as_ptr() as i32, CFG_BUF.len() as i32)
    };
    // Parse with a minimal JSON reader or use a no_std JSON crate.
    // Example: check for the string "secret-abc" directly in the bytes.
    let cfg = unsafe { &CFG_BUF[..cfg_len.max(0) as usize] };

    let key_name = b"x-api-key";
    let n = unsafe {
        conduit_get_header(
            key_name.as_ptr() as i32, key_name.len() as i32,
            HDR_BUF.as_ptr() as i32, HDR_BUF.len() as i32,
        )
    };
    if n < 0 {
        unsafe { conduit_set_response_status(401); }
        return 1;
    }

    let key = unsafe { &HDR_BUF[..n as usize] };
    // Simple substring check — use a proper JSON parser in production.
    if !cfg.windows(key.len()).any(|w| w == key) {
        unsafe { conduit_set_response_status(403); }
        return 1;
    }

    0
}
```

For a cleaner approach, add a `no_std`-compatible JSON crate like
[`miniserde`](https://crates.io/crates/miniserde) or
[`serde_json_core`](https://crates.io/crates/serde-json-core).

---

## Building a Rust plugin

### Cargo.toml

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]   # required: produces a .wasm shared library

[profile.release]
opt-level = "s"           # optimize for size
strip = true              # strip debug symbols
```

### Build

```bash
# Add the WASM target (once)
rustup target add wasm32-unknown-unknown

# Build
cargo build --target wasm32-unknown-unknown --release

# Output
ls target/wasm32-unknown-unknown/release/*.wasm
```

### Optimize (optional)

Use [`wasm-opt`](https://github.com/WebAssembly/binaryen) to reduce file size:

```bash
wasm-opt -Os -o my-plugin-opt.wasm \
  target/wasm32-unknown-unknown/release/my_plugin.wasm
```

---

## Execution model

- Modules are **compiled once** (on first request) by Wasmtime's Cranelift JIT
  and cached for the lifetime of the process. Hot-reload clears the cache.
- Each request runs in its **own Wasmtime Store** — no shared mutable state
  between requests, no global variables visible across calls.
- WASM execution is **synchronous** and runs in the request-handling thread.
- There is **no network or filesystem access** from within the WASM sandbox —
  only the 17 host functions listed above.

---

## Error handling

Conduit uses **fail-open** for WASM: if a plugin fails to load, link, or
execute (trap), the error is logged and the request passes through as if the
plugin returned `0` (continue).

```
WARN conduit::filter::wasm: WASM plugin error — request passes through (fail-open)
  plugin="./plugins/auth-check.wasm"
  error="WASM module missing 'on_request' export"
```

Common causes:
| Error | Cause |
| ----- | ----- |
| `missing 'on_request' export` | Plugin does not export `on_request` |
| `missing 'memory' export` | Plugin does not export its linear memory |
| `trap: unreachable` | Plugin panicked (e.g. array out-of-bounds) |
| `failed to read file` | Plugin path does not exist or is not readable |

---

## Comparison with Rhai

| Feature | Rhai | WASM |
| ------- | ---- | ---- |
| Compile-time feature | none (always available) | `--features wasm` |
| Language | Rhai (scripting) | Any language compiling to WASM |
| Mutate request headers | ❌ read-only | ✅ set + remove |
| Read client IP | ❌ | ✅ `conduit_get_client_ip` |
| Plugin config | ❌ | ✅ `conduit_get_plugin_config` |
| Performance | fast (interpreted) | faster (JIT-compiled) |
| Development speed | fast (no build step) | slower (compile needed) |
| Error isolation | fail-open | fail-open |
| Shared state across requests | ❌ none | ❌ none (new Store per request) |

**Use Rhai** for simple guards that only read headers and abort — fast to
write, no build step.

**Use WASM** when you need to mutate request headers, read client IP, use
plugin config, require better performance, or want to write the logic in
Rust/Go/C/AssemblyScript.
