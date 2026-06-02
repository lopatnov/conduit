# Middleware Demo

Full pipeline: Rhai API gate → WASM header injector → Rhai response enricher → WASM response tagger.

## Files

| File | Type | Phase | What it does |
|------|------|-------|--------------|
| `api-gate.rhai` | Rhai script | request | API key validation — returns 401/403 on bad key |
| `header-injector.wat` | WASM (source) | request | Injects `X-Trace-Id` + `X-Wasm-Plugin` onto upstream request |
| `response-enricher.rhai` | Rhai script | response | Adds `X-Served-By`, `X-Error-Category`; strips `Server`/`X-Powered-By` |
| `response-tagger.wat` | WASM (source) | response | Adds `X-Processed-By: wasm` to every response |

## Running the demo

```bash
# Build with WASM + Rhai support (both are used in this demo)
cargo build --features "wasm,rhai"

# Compile WAT → WASM (run once)
# Using wasm-tools (install: cargo install wasm-tools):
wasm-tools parse header-injector.wat  -o header-injector.wasm
wasm-tools parse response-tagger.wat  -o response-tagger.wasm

# Start conduit
./target/debug/conduit --config examples/middleware-demo/conduit.yaml
```

## Testing

```bash
# 1. Missing API key → 401
curl -i http://localhost:8080/
# HTTP/1.1 401  +  {"error":"Missing x-api-key","status":401}

# 2. Wrong API key → 403
curl -i -H "X-Api-Key: wrong" http://localhost:8080/
# HTTP/1.1 403  +  {"error":"Invalid x-api-key","status":403}

# 3. Correct key → 200 + injected headers
curl -i -H "X-Api-Key: demo-secret" http://localhost:8080/
# X-Trace-Id: <request-id>        ← from WASM (copied X-Request-ID)
# X-Wasm-Plugin: header-injector/1.0  ← from WASM
# X-Served-By: demo-api           ← from Rhai response script
# X-Processed-By: wasm            ← from WASM response plugin

# 4. Debug logging (WASM logs INFO when X-Debug: 1 is present)
curl -i -H "X-Api-Key: demo-secret" -H "X-Debug: 1" http://localhost:8080/
# → check conduit log output for "wasm: X-Debug hit"
```

## Integration with tests

The integration tests in `tests/middleware.rs` compile the WAT files
automatically using the bundled `wat` crate — no external toolchain needed.

## Configuring the API key

Change `config.api_key` in `conduit.yaml`:

```yaml
middleware:
  - type: script
    path: ./api-gate.rhai
    config:
      api_key: "your-secret-key-here"
      api_header: "authorization"    # use Authorization header instead
```
