# Middleware: built-in checks, Rhai scripts and WASM plugins

Conduit does most of what a gateway needs with built-in settings: IP filtering, rate limits, authentication, redirects.
When that is not enough you write your own check, either as a [Rhai script](rhai.md) (a text file, no build step) or as
a [WebAssembly plugin](wasm.md) (compiled from Rust, Go, C, AssemblyScript, …). Both are configured in the same list,
`middleware`, and this page says how that list behaves and which tool to pick.

## The `middleware` list

```yaml
sites:
  - port: 8080
    proxy: http://127.0.0.1:4000
    middleware:
      - type: script          # Rhai
        path: ./scripts/block-bots.rhai
      - type: wasm            # WebAssembly
        path: ./plugins/api-key-check.wasm
        config:               # any JSON, handed to the script or plugin
          allowed_key: "secret-abc"
      - type: script
        path: ./scripts/tag-response.rhai
        phase: response       # runs on the upstream's answer instead of the request
```

| Field | Meaning |
| --- | --- |
| `type` | `"script"` (Rhai) or `"wasm"`. An unknown value is a validation error; `ipFilter`, `rateLimit`, `auth` and `headers` are accepted but do nothing here, because those checks are the top-level settings of the same names. |
| `path` | The `.rhai` text file or the `.wasm` binary module. Required. |
| `config` | Optional JSON for the script or plugin to read. |
| `phase` | `"request"` (the default) or `"response"`. It selects the phase of a `script` entry. A `wasm` entry has no phase: its `on_request` always runs on the request and its `on_response`, if it has one, on the response. |

- **Order.** Entries run in the order written, `script` and `wasm` mixed. The first entry that answers the request ends
  the chain: the entries after it do not run for that request.
- **Where it sits.** Middleware runs after the built-in checks (IP filter, rate limit, consumers, Basic, API key, JWT,
  forward auth, redirects, fault injection) and before the request is routed to an upstream. A request the built-in
  checks refuse never reaches a script or plugin.
- **Fail-open.** A script or plugin that cannot be loaded, does not compile or link, runs out of its budget or crashes is
  logged and skipped, and the request goes on. The access checks (IP filter, credentials, JWT, forward auth) fail closed; rate limiting with a Redis store that is down
  falls back to a per-instance in-memory limit instead of refusing. **Do not make a script or a plugin the
  only guard of anything that matters**; put the real check in `consumers`, `jwtAuth`, `forwardAuth` or `ipFilter`.
- **Reload.** `conduit reload` (and `POST /reload`) re-reads edited files; a file that no longer compiles keeps its last
  good version, and to turn an entry off you remove it from the list. `conduit validate` reads and compiles every
  `script` and `wasm` entry and reports one that cannot run.
- **Build features.** `script` needs the `rhai` feature and `wasm` the `wasm` feature, both in the `full` bundle (see
  [Optional build features](configuration.md#optional-build-features)). Without the feature the entry is ignored.

## Which one

| You want | Use |
| --- | --- |
| Allow or deny by IP, credentials, JWT or an external auth service; limit the rate; redirect | The built-in setting: the access checks fail closed, and all of them are cheaper than anything you write. |
| A small rule on headers or the path: block a bot, require a header, answer a path yourself | A [Rhai script](rhai.md): no build step, edit and reload. |
| Change the request headers that go upstream, read the client IP, take the logic from a language you already use, or run compiled code | A [WASM plugin](wasm.md). |
| Run on the upstream's answer | `phase: response` in Rhai, or `on_response` in WASM. |

Limits differ: a script gets 500,000 operations per call and a plugin 10,000,000 units of fuel, and neither has network or
file access. The details are in the [Rhai guide](rhai.md) and the [WASM guide](wasm.md).
