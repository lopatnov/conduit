# Rhai Middleware

Rhai is an embedded scripting language for Rust. Conduit runs a `.rhai` script
for every request that passes through the middleware, giving you custom logic
without recompiling the binary.

> **No compile-time feature needed** — Rhai is always available.

---

## Table of Contents

- [Configuration](#configuration)
- [Script API](#script-api)
  - [request object](#request-object)
  - [response object](#response-object)
  - [Return value](#return-value)
- [Examples](#examples)
  - [Allow / deny by header](#allow--deny-by-header)
  - [Block by path](#block-by-path)
  - [Block by HTTP method](#block-by-http-method)
  - [Inject request header](#inject-request-header)
  - [Rewrite path via response](#rewrite-path-via-response)
  - [Custom JSON error](#custom-json-error)
  - [Log and pass through](#log-and-pass-through)
  - [Per-script config](#per-script-config)
- [Execution model](#execution-model)
- [Error handling](#error-handling)
- [Rhai language reference](#rhai-language-reference)

---

## Configuration

```yaml
middleware:
  - type: script
    path: ./scripts/my-check.rhai

  # Multiple scripts run in order — first false wins
  - type: script
    path: ./scripts/rate-check.rhai

  # Optional config object available inside the script as JSON (see below)
  - type: script
    path: ./scripts/auth.rhai
    config:
      allowed_token: "secret-123"
      max_body_kb: 512
```

```json
{
  "middleware": [
    { "type": "script", "path": "./scripts/my-check.rhai" },
    {
      "type": "script",
      "path": "./scripts/auth.rhai",
      "config": { "allowed_token": "secret-123" }
    }
  ]
}
```

> Scripts are loaded from the path relative to the **working directory** where
> `conduit` is started. Paths starting with `/` are absolute.

---

## Script API

Every script receives two pre-populated variables: `request` and `response`.

### `request` object

Read-only view of the incoming HTTP request.

| Property / Method | Type | Description |
| ----------------- | ---- | ----------- |
| `request.path` | `String` | Request path, e.g. `"/api/users"` |
| `request.method` | `String` | HTTP method, e.g. `"GET"`, `"POST"` |
| `request.query` | `String` | Raw query string, e.g. `"page=1&size=10"` (empty when absent) |
| `request.header("Name")` | `String` | Header value — case-insensitive; empty string when absent |

### `response` object

Used to build the response when aborting the pipeline.

| Property / Method | Type | Description |
| ----------------- | ---- | ----------- |
| `response.status` | `int` | HTTP status code to send (default: `200`) |
| `response.body` | `String` | Response body text (default: `""`) |
| `response.header("Name", "Value")` | — | Append a response header |

### Return value

| Return | Effect |
| ------ | ------ |
| `true` (or any truthy value) | Pipeline continues — request forwarded to upstream |
| *(no explicit return)* | Treated as `true` — pipeline continues |
| `false` | Pipeline stops — `response` is sent to the client |

---

## Examples

### Allow / deny by header

Check that an `Authorization` header is present. Returns `401` with a
`WWW-Authenticate` challenge when it is missing.

```rhai
// auth-check.rhai
let token = request.header("Authorization");
if token == "" {
    response.status = 401;
    response.body   = "Unauthorized";
    response.header("WWW-Authenticate", "Bearer realm=\"api\"");
    return false;
}
true
```

Check a specific bearer token:

```rhai
// bearer-token.rhai
let auth = request.header("Authorization");
if auth != "Bearer my-secret-token" {
    response.status = 403;
    response.body   = "Forbidden";
    return false;
}
true
```

---

### Block by path

Deny all requests to `/admin` from this middleware point:

```rhai
// block-admin.rhai
if request.path.starts_with("/admin") {
    response.status = 404;   // pretend it doesn't exist
    return false;
}
true
```

---

### Block by HTTP method

Allow only `GET` and `HEAD`:

```rhai
// read-only.rhai
let allowed = ["GET", "HEAD"];
if !allowed.contains(request.method) {
    response.status = 405;
    response.body   = "Method Not Allowed";
    response.header("Allow", "GET, HEAD");
    return false;
}
true
```

---

### Inject request header

Add a header before forwarding to the upstream. Returning `true` passes the
request through; any mutations to `response` are ignored when continuing.

```rhai
// add-correlation-id.rhai
//
// Note: Rhai cannot mutate request headers directly.
// Use requestTransform.setHeaders in the config for static injection,
// or WASM middleware for dynamic request header mutation.
//
// This script demonstrates a guard pattern: allow through, log.
let existing = request.header("X-Correlation-ID");
if existing == "" {
    // Can't inject a new header from Rhai — log and let it pass.
    // Use requestTransform or WASM for header injection.
}
true
```

> **Limitation:** Rhai scripts can only **read** request headers, not write
> them. To inject or remove request headers dynamically, use
> [WASM middleware](wasm.md) (`conduit_set_request_header`) or the static
> [`requestTransform`](configuration.md#request--response-transform) config.

---

### Custom JSON error

```rhai
// json-errors.rhai
let key = request.header("X-API-Key");
if key == "" {
    response.status = 401;
    response.header("Content-Type", "application/json");
    response.body = `{"error":"missing API key","status":401}`;
    return false;
}
true
```

---

### Log and pass through

Scripts have full access to Rhai's standard library. Use `print` to write to
the process stdout (not the access log):

```rhai
// debug-log.rhai
print(`[debug] ${request.method} ${request.path}`);
true
```

> For production logging, prefer structured JSON logs via `logging: json`.
> `print` is useful for development debugging only.

---

### Per-script config

When `config` is set in the middleware entry, it is available as the raw JSON
string in `request.header("x-conduit-config")` — **not yet** as a parsed
object. Parse it with Rhai's JSON functions:

```yaml
# conduit.yaml
middleware:
  - type: script
    path: ./scripts/check-key.rhai
    config:
      valid_key: "$MY_SECRET_KEY"
      blocked_paths: ["/internal", "/debug"]
```

```rhai
// check-key.rhai
// Config is passed as JSON via a special internal header.
// This feature is not yet exposed to Rhai — use WASM for config-driven
// scripts. Below is a workaround using hardcoded values.

let key = request.header("X-API-Key");
if key == "" {
    response.status = 401;
    return false;
}
true
```

> **Note:** `config` objects are fully supported in [WASM plugins](wasm.md)
> via `conduit_get_plugin_config()`. Rhai scripts currently only receive the
> script `path` — the `config` field is accepted in YAML but not yet exposed
> to the script runtime.

---

## Execution model

- Scripts are **compiled once** (on first request) and the AST is cached for
  the lifetime of the process. Hot-reload (`conduit reload`) clears the cache.
- Scripts execute **synchronously** in the request-handling thread.
- Each request gets its own `request` and `response` scope — there is **no
  shared mutable state** between requests.
- The Rhai engine runs in **safe mode** — file I/O and system calls are not
  available.

---

## Error handling

Conduit uses **fail-open**: if a script fails to compile or throws a runtime
error, the error is logged as a warning and the request passes through as if
the script returned `true`.

This means a broken script will never take down your server, but it also means
errors can silently bypass auth checks. Monitor your logs.

```
WARN conduit::filter::script: Rhai compile error: Variable not found: undefined_var
WARN conduit::filter::script: Rhai runtime error: Division by zero
```

Set `RUST_LOG=conduit=debug` to see the full error context.

---

## Rhai language reference

Rhai is a simple, Rust-like scripting language. Key features available in
Conduit scripts:

```rhai
// Variables
let x = 42;
let s = "hello";

// String interpolation
let msg = `path is ${request.path}`;

// String methods
request.path.starts_with("/api")
request.path.ends_with(".json")
request.method.to_lower() == "get"
request.header("X-Foo").len() > 0

// Arrays
let blocked = ["/admin", "/internal"];
blocked.contains(request.path)

// Conditionals
if x > 10 { ... } else { ... }

// Early return
if condition { return false; }

// Loops (rarely needed in middleware)
for item in array { ... }
```

Full language documentation: [rhai.rs/book](https://rhai.rs/book/)
