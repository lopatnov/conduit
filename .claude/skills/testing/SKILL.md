---
name: testing
description: Playbook for writing/extending tests in conduit — when to reach for unit vs integration, the project's actual mocking idioms (port 0, rcgen, serial_test, raw TcpListener), and how to keep the suite fast and parallel-safe.
---

# Testing — conduit playbook

> Adapted from a generic test-pyramid skill into conduit's *actual, observed* patterns
> (CLAUDE.md "Архитектурные решения" — "Тесты": "port 0, rcgen, serial_test для Admin API,
> mock = TcpListener без Axum"). Don't introduce new mocking frameworks — extend what's there.

## The pyramid, conduit-shaped

- **Unit tests** (`#[cfg(test)] mod tests` inline, or `tests/<module>.rs` for larger units) —
  the bulk of the 509+/586+ (`--features full`) suite. Pure logic: guard predicates, header
  transforms, strategy selection, config parsing/validation, rate-limit math, RNG (splitmix64),
  template expansion (`{{ jwt.claim }}`).
- **Integration tests** (`tests/*.rs`, often `serial_test`-guarded) — spin up a real server on
  `127.0.0.1:0`, send real HTTP requests via `reqwest`, assert on responses/headers/status.
  Used for guard chains, Admin API, TLS/mTLS, JWT/JWKS, forward-auth, consumer model, etc.
- There is **no E2E/browser layer** — conduit is a proxy binary, not a UI. The "top of the
  pyramid" here is integration tests against a real bound socket.

## Core idioms — use these, don't invent new ones

1. **Bind to port 0** — `TcpListener::bind("127.0.0.1:0")` then read back the OS-assigned port
   via `local_addr()`. Never hardcode a port (avoids collisions when tests run in parallel).
2. **`rcgen`** — generate ephemeral self-signed certs/keys in-memory for TLS/mTLS tests
   (client-cert auth, cert rotation, upstream TLS verification). No checked-in cert fixtures.
3. **`serial_test`** — `#[serial]` on tests that bind shared global state (Admin API port,
   process-wide singletons like `forward_auth_client()` `OnceLock`, env vars). Without it,
   parallel runs flake on shared resources. Apply narrowly — over-using `#[serial]` serializes
   the whole suite and kills wall-clock time.
4. **Mock upstream = raw `TcpListener`, not Axum** — when you need a fake upstream that returns
   a canned HTTP response (test retries, circuit breaker, outlier detection, mirroring), spin
   up a bare `TcpListener` + `tokio::spawn` reading/writing raw bytes. Reaching for Axum here is
   overkill and slower to set up — match the existing pattern in `tests/`.
5. **`tokio::test`** — async test fn, default multi-thread runtime unless the test needs
   single-thread determinism (then `#[tokio::test(flavor = "current_thread")]`).

## Procedure — adding/extending tests for a change

1. **Locate the right layer first.** Pure function/struct logic with no I/O → unit test next
   to the code (or in the module's existing `tests` submodule). Anything that needs a guard
   chain, real headers, real sockets, or Admin API → integration test in `tests/`.
2. **Find a sibling test to pattern-match.** conduit has 13 feature flags and many guard/filter
   tests already — there's almost always an existing test exercising a similar shape (another
   guard, another handler, another config field). Copy its setup, don't write from scratch.
3. **Feature-gate appropriately.** If the code is behind `--features X`, the test module/fn
   needs the matching `#[cfg(feature = "X")]`. Check both `cargo test` (default, minimal) and
   `cargo test --features full` paths — `/build full` runs both.
4. **Cover the fail-open/fail-closed boundary explicitly** for anything security-relevant
   (JWT, ForwardAuth, WASM, consumers) — these have documented invariants in CLAUDE.md
   ("ForwardAuth: ... unreachable=fail closed", "WASM: ... fail-open").
5. **JWT exp tests**: `jsonwebtoken` v9 has a 60s default leeway — an "expired" token test
   token must be expired by *more* than 60 seconds, or the assertion will flake green.
6. **Run it**: `/build` (delegates to `build-validator`) — runs fmt, clippy `-D warnings`,
   and `cargo test` for default + (if relevant) `--features full`.

## What makes a good test here

- **Deterministic** — no real-clock races; if timing matters (EWMA, ejection backoff,
  slow-start), inject/advance time or use generous bounds, not `sleep` + hope.
  splitmix64 RNG is seedable — use a fixed seed for reproducible fault-injection/jitter tests.
- **Parallel-safe by default** — port 0, no shared files/dirs unless `tempfile`-scoped,
  `#[serial]` only where genuinely required (see idiom 3).
  Process-wide singletons (`forward_auth_client`, `OnceLock`-based clients/caches) are the most
  common reason a test *needs* `#[serial]` — if your test touches one, check for it.
- **Asserts behavior, not implementation** — for guard-chain tests, assert on the HTTP
  response (status/headers/body), not on internal struct fields where avoidable.
- **English only** — test names, assertions messages, comments (CLAUDE.md "Language").

## Where to look for canonical examples

- Guard chain / auth: `tests/middleware.rs`, `tests/jwt*.rs`, `tests/consumers*.rs`,
  `tests/forward_auth*.rs`
- TLS/mTLS: tests using `rcgen` — search for `rcgen::` in `tests/`
- Admin API: `#[serial]`-guarded tests binding the admin port
- Mock upstream: search for `TcpListener::bind("127.0.0.1:0")` in `tests/` for the canonical
  raw-socket fake-backend pattern (retry, circuit breaker, mirroring tests use it)
- Config parsing/validation: unit tests in `src/config/` modules (`serde_path_to_error` paths)

## When NOT to add a test here

- Don't write E2E/browser tests — there's no UI to test (CLAUDE.md "No end-user UI to localize").
- Don't add a new mocking framework/dependency for something the existing idioms already cover —
  that's a `lawyer` + `business-analyst` conversation first (new dependency, possible duplication).
- Don't over-`#[serial]` — if a test can be made parallel-safe with port 0 / scoped tempdirs,
  prefer that; serialization is a last resort for genuinely shared global state.
