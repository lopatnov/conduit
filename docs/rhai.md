# Rhai Middleware

Rhai is a specific embedded scripting language (not a compilation target). You
write `.rhai` files — plain text files in Rhai syntax — and Conduit runs them
for every request that passes through the middleware. No build step, no
compiler, no toolchain required.

> **No compile-time feature needed** — Rhai is always available.

**When to use Rhai vs WASM:**

| | Rhai | WASM |
|-|------|------|
| Language | Rhai only | Rust, C, Go, AssemblyScript, … |
| Build step | None — edit and reload | Compile to `.wasm` |
| Read request headers | ✅ | ✅ |
| **Write** request headers | ❌ | ✅ |
| Read client IP | ❌ | ✅ |
| Plugin `config` access | ✅ | ✅ |
| Best for | Simple guards, path checks, auth decisions | Header mutation, high performance, complex logic |

Use **Rhai** when you want to inspect headers and allow/deny requests without
a build step. Switch to [WASM](wasm.md) when you need to add or remove headers,
or when the logic is complex enough to benefit from a compiled language.

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
  - [Check query parameters](#check-query-parameters)
  - [Redirect old paths](#redirect-old-paths)
  - [Custom JSON error](#custom-json-error)
  - [Log and pass through](#log-and-pass-through)
  - [Per-script config](#per-script-config)
- [Execution model](#execution-model)
- [Error handling](#error-handling)
- [Rhai language reference](#rhai-language-reference)

---

## Configuration

Rhai scripts are plain text files with a `.rhai` extension. Create them with
any text editor — no compiler needed.

```yaml
# conduit.yaml
middleware:
  - type: script
    path: ./scripts/my-check.rhai    # path relative to working directory

  # Multiple scripts run in order — first false wins
  - type: script
    path: ./scripts/rate-check.rhai

  # Optional config object available inside the script as `config` variable
  - type: script
    path: ./scripts/auth.rhai
    config:
      allowed_token: "secret-123"
      max_body_kb: 512
```

```json
// conduit.json
{
  "middleware": [
    { "type": "script", "path": "./scripts/my-check.rhai" },
    {
      "type": "script",
      "path": "./scripts/auth.rhai",
      "config": { "allowed_token": "secret-123", "max_body_kb": 512 }
    }
  ]
}
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

### Check query parameters

Block or restrict access based on query string values:

```rhai
// debug-guard.rhai — block ?debug=1 in production
if request.query.contains("debug=1") {
    response.status = 403;
    response.body   = "Debug mode is disabled";
    return false;
}
true
```

```rhai
// api-version-check.rhai — require ?version=2
if !request.query.contains("version=2") {
    response.status = 400;
    response.body   = "Missing required ?version=2 parameter";
    return false;
}
true
```

---

### Redirect old paths

Use a redirect response to route legacy URLs to new ones:

```rhai
// legacy-redirect.rhai
// Redirect /legacy/* → /api/*
if request.path.starts_with("/legacy/") {
    // Strip "/legacy/" prefix (8 chars) and build new URL
    let new_path = "/api/" + request.path.sub_string(8);
    response.status = 301;
    response.header("Location", new_path);
    return false;   // send the redirect response
}
true
```

```rhai
// version-redirect.rhai
// Redirect /v1/* → /v2/*
if request.path.starts_with("/v1/") {
    response.status = 308;   // Permanent Redirect, preserves method
    response.header("Location", "/v2/" + request.path.sub_string(4));
    return false;
}
true
```

> To rewrite the path **transparently** (without the client seeing a redirect),
> use `proxy.*.rewrite` rules in the config — Rhai cannot modify the forwarded
> path directly.

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

Set a `config` object in the middleware entry — it becomes a `config` variable
in the script scope. Fields map directly to Rhai values (strings, numbers,
booleans, arrays, nested objects).

```yaml
# conduit.yaml
middleware:
  - type: script
    path: ./scripts/check-key.rhai
    config:
      valid_key: "$MY_SECRET_KEY"
      blocked_paths: ["/internal", "/debug"]
      max_body_kb: 512
```

```json
// conduit.json
{
  "middleware": [
    {
      "type": "script",
      "path": "./scripts/check-key.rhai",
      "config": {
        "valid_key": "$MY_SECRET_KEY",
        "blocked_paths": ["/internal", "/debug"],
        "max_body_kb": 512
      }
    }
  ]
}
```

```rhai
// check-key.rhai — access config fields directly

// String field
let key = request.header("X-API-Key");
if key != config.valid_key {
    response.status = 401;
    return false;
}

// Array field
if config.blocked_paths.contains(request.path) {
    response.status = 403;
    return false;
}

true
```

Config types:

| YAML / JSON value | Rhai type |
| ----------------- | --------- |
| `"string"` | `String` |
| `42` | `i64` |
| `3.14` | `f64` |
| `true` / `false` | `bool` |
| `[1, 2, 3]` | `Array` |
| `{ key: val }` | `Map` — access as `config.key` |
| absent (no `config:`) | `()` (unit) — check with `config == ()` |

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

Rhai is a simple, Rust-like scripting language. Below are the patterns most
useful when writing request middleware.

### Variables and strings

```rhai
let path   = request.path;    // "/api/users"
let method = request.method;  // "GET"
let query  = request.query;   // "page=1&size=10"
let auth   = request.header("Authorization");  // "" if absent

// String interpolation
let msg = `${method} ${path} rejected`;

// String tests
path.starts_with("/api")
path.ends_with(".json")
path.contains("admin")
auth.len() > 0
auth == ""

// Case conversion
method.to_lower() == "get"
path.to_upper()

// Extract substring — sub_string(start) or sub_string(start, length)
let tail = path.sub_string(5);      // skip first 5 chars
let seg  = path.sub_string(1, 3);   // chars 1..4
```

### Checks and early return

```rhai
// Allow → return true (or just fall through)
// Deny  → set response, return false

if auth == "" {
    response.status = 401;
    response.body   = "Unauthorized";
    return false;
}

// One-liner guard
if !path.starts_with("/api") { return true; }  // not our concern
```

### Arrays

```rhai
let blocked = ["/admin", "/internal", "/debug"];
if blocked.contains(path) {
    response.status = 403;
    return false;
}

let methods = ["GET", "POST"];
if !methods.contains(method) {
    response.status = 405;
    response.header("Allow", "GET, POST");
    return false;
}
```

### Accessing config

```rhai
// config is set in conduit.yaml under middleware[].config
// Absent config → config == ()

if config == () {
    // No config provided — use fallback
    return true;
}

if config.allowed_key != request.header("X-API-Key") {
    response.status = 403;
    return false;
}

// Numeric config
if config.max_path_len < path.len() {
    response.status = 414;
    return false;
}

// Array config
if config.blocked_paths.contains(path) {
    response.status = 403;
    return false;
}
```

### Response headers and redirect

```rhai
// Add response header (only effective when returning false)
response.header("X-Reason", "blocked");
response.header("Content-Type", "application/json");

// Redirect
response.status = 302;
response.header("Location", "/new-path");
return false;
```

### Conditionals and loops

```rhai
if x > 10 { ... } else if x > 5 { ... } else { ... }

// Switch-like
let result = switch method {
    "GET"  => "read",
    "POST" => "write",
    _      => "unknown",
};

// Loops (rarely needed in middleware)
for item in array { ... }
while condition { ... }
```

Full language documentation: [rhai.rs/book](https://rhai.rs/book/)
