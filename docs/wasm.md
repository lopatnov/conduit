# WASM Middleware

A WebAssembly plugin is a small program, written in any language that compiles to WebAssembly, that Conduit runs for
every request (and, optionally, every response). It can read the request, add or remove headers, answer the request
itself, or redirect the client. It runs in a sandbox: it has no network, no files and no clock, only the
[host functions](#host-functions) listed below.

New to middleware? [Middleware overview](middleware.md) says how the `middleware` list behaves and when to
choose a built-in check, a Rhai script or a plugin.

**Which installs have it.** WASM is the `wasm` Cargo feature, part of the `full` bundle: the `-full` release
downloads, the `:latest-full` image and the npm package contain it; the `standard` binary and the default build do not
(see [Optional build features](configuration.md#optional-build-features)). Without the feature a `wasm` entry is
ignored with a warning, and **the check it performed no longer happens**.

> **A plugin that fails lets the request through (fail-open).** A plugin that cannot be loaded, does not link, runs
> out of fuel or traps is logged and skipped, and the request goes on as if the plugin had returned "continue". So a
> plugin must not be the only access check on something that matters: put the real check in `consumers`, `jwtAuth`,
> `forwardAuth` or `ipFilter`, which fail closed, and use a plugin for what is allowed to fail. `conduit validate`
> tells you about a plugin file that cannot run ([below](#testing-and-debugging)), but only a test of your own shows
> that the plugin rejects what it should.

---

## Contents

- [Limits and guarantees](#limits-and-guarantees)
- [Your first plugin](#your-first-plugin)
- [Configuration](#configuration)
- [The ABI](#the-abi): [exports](#exports), [memory](#memory-and-the-ptr-len-convention), [host functions](#host-functions), [response phase](#response-phase)
- [Recipes](#recipes)
- [Testing and debugging](#testing-and-debugging)
- [Other languages](#other-languages)
- [Stability](#stability)
- [Comparison with Rhai](#comparison-with-rhai)

---

## Limits and guarantees

| | |
| --- | --- |
| **Fail-open** | A missing or unreadable file, an invalid module, an import that does not exist, a trap, running out of fuel: the request (or response) goes through, and one warning is logged per call. The one exception is the engine itself: if WebAssembly cannot run on this machine, configuration validation refuses the config (at start-up and on a hot reload) with `middleware type "wasm" cannot run: ...`. |
| **CPU** | 10,000,000 units of fuel per call, one per executed WebAssembly instruction, counted separately for `on_request` and `on_response`. A plugin that uses it all is stopped (a trap, so fail-open). A plain header check uses well under 100,000. The call runs on the request's own thread and blocks it until it returns. |
| **Memory** | 16 MiB of linear memory per call, counting the size the module declares and what `memory.grow` adds. A module that declares more fails to instantiate (fail-open); a `memory.grow` over the limit returns `-1` to the plugin, which has to check it. |
| **No I/O** | No network, no files, no clock, no random numbers, no WASI. Importing anything outside the `"conduit"` module makes the module fail to link. |
| **No state** | Every call gets a new instance. Nothing survives from one request to the next, or from a request's `on_request` to its `on_response`. |
| **Body** | A plugin sees no request body. In `on_response` it can replace the response body, with [limits](#response-phase). |
| **Headers** | A plugin sees each header name once: if a client sends two `Cookie` lines only one value reaches `conduit_get_header`, and the order is not preserved. |
| **Buffers** | A host function that fills your buffer writes at most `buf_len` bytes and returns the count; a count equal to `buf_len` may mean the value was cut. |

---

## Your first plugin

A plugin that answers `401` to every request that has no `X-Api-Key` header, in Rust. You need
[Rust](https://rustup.rs) and the WebAssembly target: `rustup target add wasm32-unknown-unknown`.

**1. Create the project.**

```bash
cargo new --lib first-plugin && cd first-plugin
```

**2. `Cargo.toml`:** a `cdylib` is what produces a `.wasm` file.

```toml
[package]
name = "first-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[profile.release]
opt-level = "s"   # small
strip = true
```

**3. `src/lib.rs`:**

<!-- docs-check: plugin first-plugin -->
```rust
// The host functions live in the "conduit" module; without this attribute they would be looked up in "env".
#[link(wasm_import_module = "conduit")]
extern "C" {
    fn conduit_get_header(name_ptr: *const u8, name_len: i32, buf: *mut u8, buf_len: i32) -> i32;
    fn conduit_set_response_status(status: i32);
    fn conduit_set_response_body(body_ptr: *const u8, body_len: i32);
    fn conduit_log(level: i32, msg_ptr: *const u8, msg_len: i32);
}

// Called for every request: 0 lets it continue, anything else answers it with the response set below.
#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    let name = b"x-api-key"; // look-up is case-insensitive
    let mut value = [0u8; 64]; // the host writes the header value here
    let n = unsafe {
        conduit_get_header(name.as_ptr(), name.len() as i32, value.as_mut_ptr(), value.len() as i32)
    };
    if n < 0 {
        // -1: the header is not there
        let msg = b"missing X-Api-Key";
        unsafe {
            conduit_log(3, msg.as_ptr(), msg.len() as i32); // 3 = warn
            conduit_set_response_status(401);
            conduit_set_response_body(msg.as_ptr(), msg.len() as i32);
        }
        return 1; // answer the request with the status and body set above
    }
    0
}
```

How it works: a plugin and Conduit share the plugin's memory, and a string always travels as a pointer and a length.
To *read* something, the plugin gives the host a buffer (`value`) and its size; the host copies the value in and
returns how many bytes it wrote, or `-1` if there is none. To *send* something (the response body, a log line) the
plugin passes a pointer into its own memory and a length; the host copies it out before the call returns and never
keeps the pointer. The module must export its memory (`cdylib` does by default), and it must export `on_request`.

**4. Build and look at it.**

```bash
cargo build --release --target wasm32-unknown-unknown
# target/wasm32-unknown-unknown/release/first_plugin.wasm  (Cargo turns "-" into "_"; it is 379 bytes)
```

**5. `conduit.yaml`:** point a `wasm` middleware at the file.

```yaml
sites:
  - port: 8080
    proxy: http://127.0.0.1:4000
    middleware:
      - type: wasm
        path: ./target/wasm32-unknown-unknown/release/first_plugin.wasm
```

```bash
conduit validate -c conduit.yaml   # Config is valid — 1 site, 1 route.
conduit -c conduit.yaml
```

**6. Try it.**

```bash
curl -i http://127.0.0.1:8080/                       # 401 Unauthorized, body: missing X-Api-Key
curl -i -H 'X-Api-Key: anything' http://127.0.0.1:8080/   # 200, from the upstream
```

The server log shows what the plugin logged (level `3` is `warn`, which is shown by default):

```
WARN conduit_plugin_wasm::wasm: missing X-Api-Key wasm=true
```

The `401` response has `content-type: application/octet-stream`: a plugin that sends a body also sets
`conduit_set_response_header("content-type", ...)` if the client should see something else. The whole run above is
`scripts/repro-wasm-guide.sh`, which the documentation check runs against the plugins in this guide.

---

## Configuration

```yaml
middleware:
  - type: wasm
    path: ./plugins/auth-check.wasm
  - type: wasm
    path: ./plugins/validator.wasm
    config:                     # any JSON value, handed to the plugin as bytes
      max_size_kb: 512
      allowed_origins: ["https://app.example.com"]
```

| Field | Meaning |
| --- | --- |
| `type` | `"wasm"`. |
| `path` | The `.wasm` file: a **binary** core module. The text format (`.wat`), component-model binaries and WASI imports are not accepted. Relative paths are resolved from the directory Conduit is started in. |
| `config` | Optional JSON, handed to the plugin by `conduit_get_plugin_config` as compact JSON bytes. The plugin parses it itself. |
| `phase` | Has no effect on a `wasm` entry: the plugin's `on_request` runs in the request phase and its `on_response` (if it has one) in the response phase. (It does apply to `script` entries.) |

Entries run **in the order written**, `script` and `wasm` mixed; the first one that answers the request ends the chain,
and the rest do not run for that request. In the response phase every `wasm` entry's `on_response` runs.

> **Changing a plugin.** `conduit reload`, `POST /reload` and a live update from the Kubernetes provider empty the
> compiled-module cache, so the next request reads the `.wasm` file again. If the new file cannot be read, compiled or run
> (a build that stopped half-way, a bad copy, a module without `on_request`, an import that does not exist), the
> **last module that worked stays in use** and a warning is logged
> once per reload, so a bad file does not switch the plugin off. Copy the new file into place atomically (write a
> temporary file, then rename it). **To turn a plugin off, remove its `middleware` entry**: deleting the file keeps the
> last good module running until a restart. A request that is running during the reload finishes with the module it
> holds.

---

## The ABI

A plugin talks to Conduit through **exports** that Conduit calls and **host functions** it imports from the
`"conduit"` module.

### Exports

| Export | Signature | Required | |
| --- | --- | :-: | --- |
| `on_request` | `() -> i32` | **yes** | Called for every request. `0` continues, anything else answers the request (see [abort functions](#host-functions)). Required even in a plugin that only uses `on_response`; its body may be just `0`. |
| `on_response` | `(i32) -> i32` | no | Called after the upstream answered; the argument is the upstream status. The return value is ignored. See [response phase](#response-phase). An `on_response` with another type is never called; `conduit validate` says so. |
| `memory` | linear memory | when a host function touches it | Every host function that reads or writes a string needs it. A module that calls one without exporting `memory` gets empty reads and zero-byte writes, and one warning per call. |

### Memory and the `(ptr, len)` convention

- **Reading:** the plugin passes a buffer `(buf, buf_len)` in its own memory; the host writes up to `buf_len` bytes
  and returns how many it wrote (`-1` when the value does not exist, as for a missing header).
- **Writing:** the plugin passes `(ptr, len)` of bytes in its own memory; the host copies them before returning.
- The host never keeps a pointer after the call, and a `ptr`/`len` outside the plugin's memory reads as empty.
- All strings are UTF-8 bytes. Bodies are bytes and need not be UTF-8.

### Host functions

A module imports only the functions it uses. There are 20 distinct names: 17 in the request phase and 7 in the
response phase, with 4 in both.

**Read the request** (request phase only):

| Function | Returns |
| --- | --- |
| `conduit_get_method(buf, buf_len) -> i32` | HTTP method, `"GET"`, `"POST"`, … |
| `conduit_get_path(buf, buf_len) -> i32` | Path, e.g. `"/api/users"` |
| `conduit_get_query(buf, buf_len) -> i32` | Raw query string; empty when there is none |
| `conduit_get_uri(buf, buf_len) -> i32` | Path, then `"?"` and the query when there is one |
| `conduit_get_client_ip(buf, buf_len) -> i32` | Client IP address |
| `conduit_get_request_id(buf, buf_len) -> i32` | The `X-Request-ID` value (empty when not set yet) |
| `conduit_get_header(name_ptr, name_len, buf, buf_len) -> i32` | A header value, case-insensitive name; `-1` if absent |
| `conduit_get_header_count() -> i32` | Number of distinct header names |
| `conduit_get_header_names(buf, buf_len) -> i32` | Distinct names, newline-separated, in no guaranteed order |

**Change the request** (request phase only). Applied to the upstream request after `on_request` returns `0`; ignored if the plugin answers the request instead.

| Function | Effect |
| --- | --- |
| `conduit_set_request_header(name_ptr, name_len, val_ptr, val_len)` | Add or overwrite a request header |
| `conduit_remove_request_header(name_ptr, name_len)` | Remove a request header |

**Answer the request.** Call these, then return a non-zero value from `on_request`. They have no effect when the plugin returns `0`.

| Function | Effect |
| --- | --- |
| `conduit_set_response_status(status)` | Status code, clamped to 100–999. **Default `500`** if never called. |
| `conduit_set_response_header(name_ptr, name_len, val_ptr, val_len)` | Add a header to the answer (in both phases, but [see the response phase](#response-phase)) |
| `conduit_set_response_body(body_ptr, body_len)` | The body (bytes) |
| `conduit_abort_with_redirect(url_ptr, url_len)` | `302`, `Location: <url>`, body `Redirecting...`. Still return non-zero. |

**Both phases:**

| Function | Effect |
| --- | --- |
| `conduit_get_plugin_config(buf, buf_len) -> i32` | The `config` JSON as bytes; `0` bytes when none is configured |
| `conduit_log(level, msg_ptr, msg_len)` | Write to the log; `0` trace, `1` debug, `2` info, `3` warn, `4` error (anything else is logged as error). The default log level is `warn`; see [debugging](#testing-and-debugging) |

### Response phase

A plugin may export `on_response(status: i32) -> i32`. Conduit calls it after the upstream answered, before the client
gets the response. Seven host functions work there:

| Function | Effect |
| --- | --- |
| `conduit_get_response_status() -> i32` | The upstream status |
| `conduit_get_response_header(name_ptr, name_len, buf, buf_len) -> i32` | An upstream response header; `-1` if absent |
| `conduit_set_response_header(name_ptr, name_len, val_ptr, val_len)` | Add or overwrite a header on the response the client gets |
| `conduit_remove_response_header(name_ptr, name_len)` | Remove a header from it |
| `conduit_set_response_body(body_ptr, body_len)` | Replace the body (below) |
| `conduit_get_plugin_config`, `conduit_log` | As in the request phase |

**One module, both phases.** A module may import functions of both phases. A function that belongs to the *other*
phase links, and traps (so fails open) only if the plugin calls it there: `conduit_get_header` in `on_response` is
not available, because the request is gone by then. Before issue #565 such a module did not instantiate in either
phase and was skipped on every request and every response.

**Replacing the body.** `conduit_set_response_body` in `on_response` replaces the body the client receives (issue
#379). Conduit drops the upstream `Content-Encoding`, `Transfer-Encoding`, `ETag` and `Content-MD5` headers and sets
`Content-Length` to the new length; if several plugins set a body, the last one wins. Limits:

- A response that cannot carry a body (`1xx`, `204`, `205`, `304`, or an upstream `Content-Length: 0`) is sent as the
  upstream sent it, and Conduit logs one warning (one per process, not per route).
- `on_response` does not run for a `5xx` that `maskErrors` masks or a retry replaces.
- Whether the upstream sent an empty body is decided from its own `Content-Length` before any `responseTransform`
  runs. An HTTP/2 upstream that ends the stream on the HEADERS frame without a `Content-Length` looks like a response
  with a body, so the replacement is applied and the response is inconsistent. This is a known limitation.
- Headers that describe the old body (`Content-Range`, `Accept-Ranges`, `Content-Digest`) are left alone: a plugin
  that replaces a `206` or a digested body removes them with `conduit_remove_response_header`.
- If the plugin traps or runs out of fuel the original body goes through, so **`on_response` is not a redaction
  guarantee**. These gaps are tracked in #560.

---

## Recipes

Each plugin below is the whole `src/lib.rs` of a crate set up like the [first plugin](#your-first-plugin) (the same
`Cargo.toml` with another `name`). The documentation check builds each one and runs `conduit validate` on it.

### Add a header to the upstream request

<!-- docs-check: plugin inject-header -->
```rust
#[link(wasm_import_module = "conduit")]
extern "C" {
    fn conduit_set_request_header(name_ptr: *const u8, name_len: i32, val_ptr: *const u8, val_len: i32);
}

#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    let (name, value) = (b"x-plugin-version", b"1.0");
    unsafe {
        conduit_set_request_header(name.as_ptr(), name.len() as i32, value.as_ptr(), value.len() as i32);
    }
    0 // continue: the header is added before the request goes upstream
}
```

### Redirect old paths

<!-- docs-check: plugin path-redirect -->
```rust
#[link(wasm_import_module = "conduit")]
extern "C" {
    fn conduit_get_path(buf: *mut u8, buf_len: i32) -> i32;
    fn conduit_abort_with_redirect(url_ptr: *const u8, url_len: i32);
}

#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    let mut path = [0u8; 512];
    let n = unsafe { conduit_get_path(path.as_mut_ptr(), path.len() as i32) };
    if n > 0 && path[..n as usize].starts_with(b"/old-api/") {
        let target = b"/api/";
        unsafe { conduit_abort_with_redirect(target.as_ptr(), target.len() as i32) };
        return 1; // answer with the 302
    }
    0
}
```

### Read the plugin `config`

```yaml
middleware:
  - type: wasm
    path: ./plugins/key-from-config.wasm
    config:
      allowed_key: "secret-abc"
```

The plugin receives `{"allowed_key":"secret-abc"}` as bytes. This one reads a flat string value by hand to stay free
of dependencies (it does not handle escapes; in a real plugin use a `no_std`-friendly JSON crate such as
[`serde-json-core`](https://crates.io/crates/serde-json-core)), and refuses every request when no key is configured.

<!-- docs-check: plugin key-from-config -->
```rust
#[link(wasm_import_module = "conduit")]
extern "C" {
    fn conduit_get_plugin_config(buf: *mut u8, buf_len: i32) -> i32;
    fn conduit_get_header(name_ptr: *const u8, name_len: i32, buf: *mut u8, buf_len: i32) -> i32;
    fn conduit_set_response_status(status: i32);
}

/// The string value of `key` in a flat JSON object such as `{"key":"abc"}`.
fn json_str<'a>(json: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let needle = [b"\"", key.as_bytes(), b"\":\""].concat();
    let start = json.windows(needle.len()).position(|w| w == needle.as_slice())? + needle.len();
    let len = json[start..].iter().position(|&b| b == b'"')?;
    Some(&json[start..start + len])
}

#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    let mut config = [0u8; 1024];
    let n = unsafe { conduit_get_plugin_config(config.as_mut_ptr(), config.len() as i32) };
    let Some(allowed) = json_str(&config[..n.max(0) as usize], "allowed_key") else {
        unsafe { conduit_set_response_status(503) }; // not configured: refuse rather than let everything in
        return 1;
    };

    let name = b"x-api-key";
    let mut given = [0u8; 256];
    let m = unsafe { conduit_get_header(name.as_ptr(), name.len() as i32, given.as_mut_ptr(), given.len() as i32) };
    // A plain comparison; use a constant-time one for real secrets.
    if m < 0 || &given[..m as usize] != allowed {
        unsafe { conduit_set_response_status(403) };
        return 1;
    }
    0
}
```

### A gate and a response tag in one plugin

`on_request` rejects requests without a key; `on_response` marks error responses. The module imports a function of
each phase and works in both.

<!-- docs-check: plugin gate-and-tag -->
```rust
#[link(wasm_import_module = "conduit")]
extern "C" {
    fn conduit_get_header(name_ptr: *const u8, name_len: i32, buf: *mut u8, buf_len: i32) -> i32;
    fn conduit_set_response_status(status: i32);
    fn conduit_get_response_status() -> i32;
    fn conduit_set_response_header(name_ptr: *const u8, name_len: i32, val_ptr: *const u8, val_len: i32);
}

#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    let name = b"x-api-key";
    let mut value = [0u8; 8];
    let n = unsafe { conduit_get_header(name.as_ptr(), name.len() as i32, value.as_mut_ptr(), value.len() as i32) };
    if n < 0 {
        unsafe { conduit_set_response_status(401) };
        return 1;
    }
    0
}

#[no_mangle]
pub extern "C" fn on_response(_status: i32) -> i32 {
    if unsafe { conduit_get_response_status() } >= 500 {
        let (name, value) = (b"x-error-seen", b"1");
        unsafe {
            conduit_set_response_header(name.as_ptr(), name.len() as i32, value.as_ptr(), value.len() as i32);
        }
    }
    0 // the return value of on_response is ignored
}
```

---

## Testing and debugging

**Before it runs: `conduit validate`.** It reads, compiles and links every `wasm` entry without running it, and
reports a file that cannot work, because at run time such a plugin is skipped and the request passes through:

```
$ conduit validate -c conduit.yaml
error at sites[0].middleware[0].path: plugin "./absent.wasm" cannot be read (entity not found); until that is fixed
this entry is skipped and requests pass through it (after a reload, a version that already ran in this process
stays in use)
```

It also names a file that is not a regular file or is over 64 MiB, is not a binary module, exports neither `on_request` nor
`on_response` with the type the host calls, imports a function that does not exist or has the wrong type (a misspelt
`conduit_get_hedaer`, a WASI import), or declares a shared memory or an exported memory whose initial size is over the
16 MiB limit: all of those make the plugin fail to load or to instantiate, so it is skipped. Three problems leave a
plugin that still runs, and the warning says so: a module that imports host functions that need memory but exports none
(those calls read and write nothing), an `on_response` of the wrong type (it is never called), and a missing
`on_request` in a plugin that has an `on_response` (the request phase logs a warning on every request). A running server only logs these at
start-up and on reload (a warning there, so that a file can be put in place after the configuration); `conduit
validate` exits non-zero. What it cannot tell: whether the plugin decides correctly.

**Seeing what a plugin does.** `conduit_log` lines appear in the server log as
`conduit_plugin_wasm::wasm: <message> wasm=true`. The default level is `warn`, so `info` and below need
`RUST_LOG=conduit_plugin_wasm=debug`. A failing plugin logs why, once per call:

```
WARN conduit_plugin_wasm::wasm: WASM plugin error — request passes through (fail-open) plugin="./absent.wasm" error=No such file or directory (os error 2)
WARN conduit_plugin_wasm::wasm: WASM on_response error — response passes through (fail-open) plugin="./absent.wasm" error=...
```

A plugin that traps logs it in the same line, as `error=error while executing at wasm backtrace:` followed by
the function index (a module built with names shows them). To see one, build this:

<!-- docs-check: plugin always-traps -->
```rust
#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    panic!("boom") // a panic aborts: in WebAssembly that is the `unreachable` instruction, a trap
}
```

**Testing the logic without a proxy.** Keep the decision in a function that takes plain values and test that with
`cargo test` on your own machine; the host functions stay in a thin shim that only exists for `wasm32`:

<!-- docs-check: plugin testable -->
```rust
/// What to do with a request, decided from plain values.
pub fn decide(api_key: Option<&[u8]>) -> Option<i32> {
    match api_key {
        None => Some(401),            // answer with 401
        Some(k) if k.is_empty() => Some(403),
        Some(_) => None,              // let it through
    }
}

#[cfg(target_arch = "wasm32")]
mod shim {
    #[link(wasm_import_module = "conduit")]
    extern "C" {
        fn conduit_get_header(name_ptr: *const u8, name_len: i32, buf: *mut u8, buf_len: i32) -> i32;
        fn conduit_set_response_status(status: i32);
    }

    #[no_mangle]
    pub extern "C" fn on_request() -> i32 {
        let name = b"x-api-key";
        let mut value = [0u8; 64];
        let n = unsafe { conduit_get_header(name.as_ptr(), name.len() as i32, value.as_mut_ptr(), value.len() as i32) };
        let key = if n < 0 { None } else { Some(&value[..n as usize]) };
        match super::decide(key) {
            Some(status) => {
                unsafe { conduit_set_response_status(status) };
                1
            }
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decide;

    #[test]
    fn a_request_without_a_key_is_refused() {
        assert_eq!(decide(None), Some(401));
        assert_eq!(decide(Some(b"")), Some(403));
        assert_eq!(decide(Some(b"k")), None);
    }
}
```

A final check, with the real host, is a request through a running Conduit as in [Your first plugin](#your-first-plugin);
the plugin's `.wasm` file can also be fed straight to `conduit validate`.

---

## Other languages

Any language that compiles to a freestanding `wasm32` module works if it can (1) import functions from the module
`conduit`, (2) export `on_request` and its memory, and (3) not import anything else. The Rust samples above are
built by the documentation check; **the samples below are not** (C and Go need toolchains the check does not install),
except that the AssemblyScript one was built and validated by hand when this guide was written.

| Language | Toolchain | Notes |
| --- | --- | --- |
| **Rust** | `cargo build --target wasm32-unknown-unknown --release` | `#[link(wasm_import_module = "conduit")]` on the `extern` block. `crate-type = ["cdylib"]`. |
| **C / C++** | `clang --target=wasm32 -nostdlib -Wl,--no-entry` | `__attribute__((import_module("conduit"), import_name("...")))` on every import. |
| **Go** | [TinyGo](https://tinygo.org/) `-target=wasm-unknown` | `//go:wasmimport conduit <name>`. `-target=wasi`/`wasip1` imports `wasi_snapshot_preview1` and does not link; `-target=wasm` is browser-oriented. |
| **AssemblyScript** | `npx asc` | `@external("conduit", "<name>")`. |
| **Zig** | `zig build-lib -target wasm32-freestanding -dynamic` | Not tried for this guide. |

An API-key check (`401` when `X-Api-Key` is missing) in each:

**C**

```c
// plugin.c — build: clang --target=wasm32 -nostdlib -Wl,--no-entry -Wl,--export=on_request -Wl,--export=memory -o plugin.wasm plugin.c
#define IMPORT(name) __attribute__((import_module("conduit"), import_name(name)))
IMPORT("conduit_get_header") extern int conduit_get_header(const char *name, int name_len, char *buf, int buf_len);
IMPORT("conduit_set_response_status") extern void conduit_set_response_status(int status);

static char buf[256];

__attribute__((export_name("on_request")))
int on_request(void) {
    if (conduit_get_header("x-api-key", 9, buf, sizeof buf) < 0) {
        conduit_set_response_status(401);
        return 1;
    }
    return 0;
}
```

**Go (TinyGo)**

```go
// plugin.go — build: tinygo build -o plugin.wasm -target=wasm-unknown ./plugin.go
package main

import "unsafe"

//go:wasmimport conduit conduit_get_header
func conduitGetHeader(namePtr, nameLen, bufPtr, bufLen uint32) int32

//go:wasmimport conduit conduit_set_response_status
func conduitSetResponseStatus(status int32)

var buf [256]byte

//export on_request
func onRequest() int32 {
	name := []byte("x-api-key")
	n := conduitGetHeader(
		uint32(uintptr(unsafe.Pointer(&name[0]))), uint32(len(name)),
		uint32(uintptr(unsafe.Pointer(&buf[0]))), uint32(len(buf)))
	if n < 0 {
		conduitSetResponseStatus(401)
		return 1
	}
	return 0
}

func main() {}
```

**AssemblyScript**

```typescript
// plugin.ts — build: npx asc plugin.ts --target release --outFile plugin.wasm --exportRuntime --use abort=
@external("conduit", "conduit_get_header")
declare function conduit_get_header(namePtr: i32, nameLen: i32, buf: i32, bufLen: i32): i32;
@external("conduit", "conduit_set_response_status")
declare function conduit_set_response_status(status: i32): void;

const buf = new ArrayBuffer(256);

export function on_request(): i32 {
  const name = String.UTF8.encode("x-api-key");
  const n = conduit_get_header(changetype<i32>(name), name.byteLength, changetype<i32>(buf), 256);
  if (n < 0) {
    conduit_set_response_status(401);
    return 1;
  }
  return 0;
}
```

**WebAssembly text (WAT).** The smallest plugin, for reading the ABI without a language on top. Assemble it first
(`wasm-tools parse minimal.wat -o minimal.wasm` or `wat2wasm`): Conduit loads only the binary form.

```wat
(module
  (memory (export "memory") 1)
  (func (export "on_request") (result i32)
    i32.const 0))   ;; 0 = continue
```

A build can be made smaller with [`wasm-opt`](https://github.com/WebAssembly/binaryen) (`wasm-opt -Os -o out.wasm in.wasm`).

---

## Stability

There is no ABI version export and no formal compatibility promise yet. What holds today: the host functions in this
guide are what this release ships; a module imports only what it uses, so a function added in a later release does not
affect modules that do not import it; a function that is removed or retyped makes a module that imports it fail to
link, which `conduit validate` reports (and the request then passes through, fail-open). Plan for the ABI as
pre-1.0: changes are listed in [CHANGELOG.md](../CHANGELOG.md). The host runs plugins on Wasmtime with its
Cranelift compiler; a module may use reference types, bulk memory, multi-value, SIMD and the atomics instructions, but a
module that declares a *shared* memory does not instantiate.

---

## Comparison with Rhai

| | Rhai | WASM |
| --- | --- | --- |
| Compile-time feature | `rhai` | `wasm` |
| Language | Rhai scripting | Any language that compiles to WebAssembly |
| Change request headers | no (`headers` is read-only) | yes: set and remove |
| Read the client IP | no | yes: `conduit_get_client_ip` |
| Plugin config | yes | yes: `conduit_get_plugin_config` |
| CPU limit per call | 500,000 operations | 10,000,000 fuel units |
| State across requests | none | none |
| On failure | fail-open | fail-open |
| Development | no build step | a build step |

Use **Rhai** for a guard that reads headers and answers or lets the request through: no build step. Use **WASM** when
you need to change request headers, read the client IP, take the logic from a language you already use, or need the
speed of compiled code. See the [Rhai guide](rhai.md).
