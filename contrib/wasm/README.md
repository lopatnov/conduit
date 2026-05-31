# WASM Plugin SDK for Conduit

Write middleware plugins in any language that compiles to WebAssembly.

## Quick start

```yaml
# conduit.yaml
middleware:
  - type: wasm
    path: ./my-plugin.wasm
    config: { "apiKey": "secret" }
```

## Building a plugin

### From WAT (WebAssembly Text Format)

```bash
# Install wabt toolchain
wabt/bin/wat2wasm contrib/wasm/example-auth/plugin.wat -o plugin.wasm
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
// src/lib.rs  (target: wasm32-unknown-unknown)
extern "C" {
    fn conduit_get_header(name: *const u8, name_len: i32, buf: *mut u8, buf_len: i32) -> i32;
    fn conduit_set_response_status(status: i32);
    fn conduit_set_response_body(body: *const u8, body_len: i32);
}

#[no_mangle]
pub extern "C" fn on_request() -> i32 {
    let key_header = b"x-api-key";
    let mut buf = [0u8; 256];
    let n = unsafe {
        conduit_get_header(key_header.as_ptr(), key_header.len() as i32,
                           buf.as_mut_ptr(), 256)
    };
    if n < 0 {
        unsafe { conduit_set_response_status(401) };
        let msg = b"missing api key";
        unsafe { conduit_set_response_body(msg.as_ptr(), msg.len() as i32) };
        return 1; // Abort
    }
    0 // Continue
}
```

```bash
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/my_plugin.wasm .
```

## ABI reference

See `sdk/conduit.wat` for the full list of 17 host functions.

| Category | Functions |
|----------|-----------|
| Request read | `get_method`, `get_path`, `get_query`, `get_uri`, `get_client_ip`, `get_request_id`, `get_header`, `get_header_count`, `get_header_names`, `get_plugin_config` |
| Request mutate | `set_request_header`, `remove_request_header` |
| Abort response | `set_response_status`, `set_response_header`, `set_response_body`, `abort_with_redirect` |
| Logging | `log` |

## Examples

- `example-auth/plugin.wat` — API key check + 302 redirect demo

## Plugin contract

- Must export `on_request() → i32` (0 = Continue, 1 = Abort)
- Must export `"memory"`
- Fail-open: any compile/link/trap error lets the request pass through
