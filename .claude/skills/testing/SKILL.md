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
6. **Suspect a hash-distribution/self-mapping bug? Measure it empirically before designing
   the fix or the regression test.** Write a throwaway `#[cfg(test)] mod tmp_probe` that
   calls the **real production functions** directly (not a reimplementation of them) across
   a sweep of realistic inputs, `println!`/`eprintln!` the result with `-- --nocapture`, then
   delete the probe before committing — `git diff`/`git status` to confirm it's gone. This is
   how issue #220's actual severity was found (a sticky-session HMAC pin was assumed to route
   correctly; measuring `fnv1a_hash(url) % ring.len() == index_of(url)` across 2..8-peer rings
   with the real `hash_pick_bounded` showed ~23% — chance level — and exactly 0% at 4 peers),
   and it's also the fastest way to *pick fixture values* for the eventual regression test —
   see the next idiom below, which exists because skipping this step led straight into it.
7. **Raw-TCP-client tests (test acting as the *client* against a real server) — don't
   `stream.shutdown()` after writing the request.** Confirmed on this dev machine's Windows
   toolchain (2026-08-31, issue #286's regression test): a client half-closing its write side
   before the server's `accept()` has run can drop the connection's still-pending backlog entry
   outright — `read_to_end` then returns a clean `Ok(0)` (not an error), which looks exactly
   like "the server responded with nothing" and is easy to misdiagnose as a server-side bug.
   Send a correct `Content-Length` and let the *server's* own `Connection: close` (or an
   explicit read-until-expected-bytes) end the exchange — don't rely on the client signaling
   EOF via a write-side shutdown. If a client-role test in this repo ever gets a mysterious
   empty response with no error, this is the first thing to check before suspecting the code
   under test.

## Procedure — adding/extending tests for a change

1. **Locate the right layer first.** Pure function/struct logic with no I/O → unit test next
   to the code (or in the module's existing `tests` submodule). Anything that needs a guard
   chain, real headers, real sockets, or Admin API → integration test in `tests/` — **except**
   see the coverage-driven carve-out below, added 2026-08-23.

### Narrow exception: `Session`-touching code that specifically needs `--lib` coverage credit

`cargo llvm-cov --lib` (the command `.github/workflows/sonar.yml` uses to feed SonarCloud's
"Coverage on New Code" gate) only instruments unit tests — `tests/*.rs` integration-test
binaries are never measured (see issue #248: they're deliberately excluded because
instrumenting a real spawned server can hang CI for hours). That means a single-guard/handler
function that only ever gets exercised through an integration test shows as 0% covered on
SonarCloud even when it's genuinely tested end-to-end.

For that specific situation — not as a general replacement for integration tests — write an
inline `#[cfg(test)] mod tests` unit test that builds a real `pingora_proxy::Session` directly,
instead of routing everything through `tests/`:

```rust
async fn session_with_request() -> (pingora_proxy::Session, tokio::io::DuplexStream) {
    let (server_side, mut client_side) = tokio::io::duplex(4096);
    client_side.write_all(b"GET /test HTTP/1.1\r\nHost: test\r\n\r\n").await.unwrap();

    let stream: pingora_core::protocols::Stream = Box::new(server_side);
    let mut session = pingora_proxy::Session::new_h1(stream);
    session.as_downstream_mut().read_request().await.expect("read_request");

    (session, client_side)
}
```

Why this is legitimate, not a hack: `pingora_proxy::Session::new_h1` is a **public** constructor
whose own doc comment says it's "mostly used for testing and mocking", and `pingora-core`
implements its internal `IO` trait for `tokio::io::DuplexStream` specifically for this purpose
(`pingora_core::protocols::mod::ext_io_impl`, comment: "mostly for testing"). No real socket,
no private API reached into — just the in-memory pipe primitive Pingora built for exactly this.
Write the client-side request bytes into `client_side` *before* `read_request()`, and read the
guard's response back from `client_side` afterward to assert on real status/headers/body, not
just the `FilterOutcome` enum. Needs `pingora-proxy` as a `[dev-dependencies]` entry in that
crate's `Cargo.toml` (it's normally only a root-crate dependency) — see `crates/conduit-faults/
Cargo.toml` for the first example (guard.rs's `FaultInjectionGuard` tests).

This does **not** replace integration-test coverage for realistic multi-guard chain / routing /
config-driven behavior — it's specifically for closing a coverage gap on one function's
isolated logic when the file's whole purpose is otherwise well-tested end-to-end already.
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

## Negative controls need a fixture that can actually fail

This repo's established discipline for a regression test is: temporarily revert the fix,
confirm the new test fails with the pre-fix symptom, restore the fix, confirm it passes.
Running that cycle is necessary but **not sufficient** — the fixture values also have to be
chosen so buggy and correct code genuinely produce different answers. Two adjacent PRs
(#372, #373) hit this exact trap **four separate times**:

- A test for a retry attempt's forward-probe used `attempt=1` on a 2-URL list. `base =
  attempt % len` happened to already equal the *correct*, post-probe answer, so the test
  passed with the probe logic completely disabled — a change to `attempt=2` (where the naive
  index lands on the deliberately-saturated peer) was needed before the test meant anything.
- A test for "attempt 0 must not touch the conn-slot machinery" used a same-peer,
  same-tracked-value scenario, where a broken implementation (touching the slot when it
  shouldn't) produced the *identical final state* as the correct one (release-then-reacquire
  on the same peer, same flag, nets out to a no-op either way). Fixed by deliberately setting
  up a state where routing's own decision *disagrees* with what the buggy code path would
  compute, so a wrong implementation is forced to visibly diverge.
- A sticky-session pin test used a 2-peer ring, which — per the empirical measurement in idiom
  6 above — is one of the ~23% of rings where a peer's URL happens to hash back to its own
  index. The bug (hashing the pin instead of honoring it directly) was invisible at n=2 and
  only surfaced once the fixture swept every (ring size, pinned index) pair.
- A **pre-existing** test from an earlier PR (#156) had the same 2-peer blind spot and was
  still passing even after the new bug was deliberately, artificially reintroduced during
  review — caught by `security-engineer`, not by the author. Re-pointing the fixture to a
  3-peer ring at one of the measured "wrong" indices made it discriminate.

**The check, concretely**: after running the standard revert→fail→restore→pass cycle, ask
"would this same fixture also pass if I patched in the specific wrong behavior I'm actually
worried about, rather than just deleting the fix wholesale?" For anything hash/modulo/
rotation-index-based, that means picking inputs already known (from idiom 6's empirical
measurement) to be on the *wrong* side of the naive computation, not just any input.

**Also verify the negative control itself actually landed.** A scripted find-and-replace
(`perl -0pi`, `sed`) used to simulate the bug can silently no-op — this happened in this same
work because `cargo fmt` had reflowed the target line onto two lines between when the pattern
was written and when it ran, so the "negative control" changed nothing and the suite came
back green for the wrong reason (looked like "test doesn't catch the bug" when really nothing
had been patched). A second such substitution, missing a `/g` flag, collaterally changed an
unrelated test's fixture line elsewhere in the same file. Before trusting a negative-control
result: `git diff` the working tree and *read* the change, don't just look at the test output.
For anything beyond a single unambiguous line, editing by hand (or with enough surrounding
context to guarantee a unique match) is safer than a scripted regex pass.

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
