# Running Node.js / Python apps behind Conduit as a worker pool

## The idea in one sentence

Conduit does what a reverse proxy does best — TLS, routing, load balancing,
rate limiting, health checking, retries — and hands the actual request
handling off to a pool of Node.js or Python worker processes running your
application code.

## What Conduit already does for you, today, with zero new code

- `proxy:`/`routes:` path/host/header/query matching — route specific paths to
  your worker pool while other paths go elsewhere (static files, a different
  upstream, etc.).
- 8 load-balancing strategies (round-robin, least-connections, IP hash, P2C,
  weighted round-robin, ...) across however many worker instances you run.
- TLS termination, rate limiting, circuit breaking, passive health tracking
  (EWMA latency, outlier detection), retries with jitter — all in front of
  your worker pool, none of it your application code's problem.
- `X-Request-ID` injection (`XRequestIdGuard`) on every request, so you can
  correlate a request across Conduit's own access log and your application's
  logs.

None of that requires anything beyond a normal `proxy:` config pointing at
`http://127.0.0.1:PORT`. **What's missing** is _pool management_: who starts
N worker processes, watches their health, restarts crashed ones, and tells
Conduit when a worker comes up or goes away. That's what this recipe adds.

## The mechanism: Conduit's dynamic upstream Admin API

`conduit upstreams add/remove/weight` (the CLI subcommands) are thin clients
over three real HTTP endpoints on the Admin API (bound to `global.admin.bind`,
e.g. `127.0.0.1:2019`):

```text
POST /upstreams/add     {"route": "/api", "target": "http://127.0.0.1:4001", "weight": 1, "site": "*:8080"}
POST /upstreams/remove  {"route": "/api", "target": "http://127.0.0.1:4001", "site": "*:8080"}
POST /upstreams/weight  {"route": "/api", "target": "http://127.0.0.1:4001", "weight": 3, "site": "*:8080"}
```

- `route` — the path prefix from your `proxy:`/`routes:` config this target
  serves.
- `target` — the worker's full URL.
- `site` — optional; scopes the override to one site (`"{host}:{port}"`).
  Omit it for a single-site deployment (the examples below do) — the
  registration then applies to every site serving this route.
- If `global.admin.token` is configured, include
  `Authorization: Bearer <token>` on every call. **Set one** if anything
  else runs on the same host as Conduit — without a token, any local
  process can call `/upstreams/add`, `/reload`, or other Admin API
  endpoints (the loopback bind keeps this off the network, but not away
  from other processes on the same machine). The examples below already
  read it from `CONDUIT_ADMIN_TOKEN`/`$CONDUIT_ADMIN_TOKEN` when set.
- Registrations are **in-memory only** — `conduit reload` (re-reading the
  config file from disk, for _any_ config change, not just one related to
  this route) clears every dynamic registration, immediately, even if the
  supervisor itself keeps running. A supervisor that only registers once at
  startup would silently fall back to the config's static seed target until
  it's restarted. The examples below avoid this by re-issuing
  `/upstreams/add` on a periodic timer for every worker they still consider
  alive — `/upstreams/add` is idempotent (it updates the existing entry's
  weight in place, never duplicates), so this is safe to do repeatedly.

Because these are plain HTTP endpoints, **any process in any language** can
call them directly — not just the `conduit` binary's own CLI. That's the
whole trick: a small supervisor script in your worker pool's own language
calls `/upstreams/add` when a worker becomes ready and `/upstreams/remove`
when it exits, and Conduit's existing load balancer does the rest.

## What actually runs

Three kinds of process, side by side:

1. **`conduit`** (Rust) — listens on the public port, routes, load-balances,
   handles TLS/rate-limiting/health-checks.
2. **A supervisor process** (your code, in Node.js or Python) — starts N
   worker processes, watches them, restarts crashed ones, registers/
   deregisters them with Conduit via the Admin API above.
3. **N worker processes** (your code) — the actual application, one instance
   per process, each on its own loopback port.

Request flow: client → `conduit:8080` (TLS, rate limit, route match, pick a
healthy worker via the configured strategy) → one of the live worker
processes (your application logic) → response flows back through Conduit.
Conduit already owns the balancing/health/circuit-breaker decisions; the
supervisor's only job is keeping the worker list accurate.

## Node.js example

```js
// pool.js — worker-pool supervisor
const { fork } = require("child_process");
const http = require("http");

const NUM_WORKERS = 4;
const BASE_PORT = 4001;
const ADMIN_URL = "http://127.0.0.1:2019";
const ADMIN_TOKEN = process.env.CONDUIT_ADMIN_TOKEN; // unset if global.admin.token isn't configured
const ROUTE = "/api"; // must match a path prefix in conduit.yaml
// No `site` field below — this example is single-site, so the registration
// applies to whichever site serves ROUTE. Add `site: "host:port"` for a
// multi-site deployment.

function callAdmin(path, body) {
  return new Promise((resolve, reject) => {
    const data = JSON.stringify(body);
    const headers = {
      "Content-Type": "application/json",
      "Content-Length": Buffer.byteLength(data),
    };
    if (ADMIN_TOKEN) headers["Authorization"] = `Bearer ${ADMIN_TOKEN}`;
    const req = http.request(
      ADMIN_URL + path,
      { method: "POST", headers },
      (res) => {
        let chunks = "";
        res.on("data", (c) => (chunks += c));
        res.on("end", () => {
          if (res.statusCode < 200 || res.statusCode >= 300) {
            // A 401 (missing/wrong admin token) returns an empty body --
            // reject before JSON.parse would throw on it.
            reject(
              new Error(
                `Admin API ${path} returned HTTP ${res.statusCode}: ${chunks}`,
              ),
            );
            return;
          }
          resolve(chunks ? JSON.parse(chunks) : {});
        });
      },
    );
    req.on("error", reject);
    req.write(data);
    req.end();
  });
}

// Registers `target` and retries a few times on failure (Admin API
// momentarily unreachable, etc.) rather than leaving a live worker
// silently unregistered. The periodic re-register timer below is the
// longer-term backstop — this is just for the very first attempt.
async function registerWithRetry(target, attempts = 5, delayMs = 1000) {
  for (let i = 1; i <= attempts; i++) {
    try {
      await callAdmin("/upstreams/add", { route: ROUTE, target, weight: 1 });
      return true;
    } catch (err) {
      console.error(
        `worker ${target} registration attempt ${i}/${attempts} failed: ${err.message}`,
      );
      if (i < attempts) await new Promise((r) => setTimeout(r, delayMs));
    }
  }
  return false;
}

function spawnWorker(port) {
  const worker = fork("./worker.js", [], {
    env: { ...process.env, PORT: port },
  });
  const target = `http://127.0.0.1:${port}`;
  let reregisterTimer = null;

  worker.on("message", async (msg) => {
    if (msg === "ready") {
      const ok = await registerWithRetry(target);
      if (!ok) {
        console.error(
          `worker ${port}: giving up on registration, killing and respawning`,
        );
        worker.kill();
        return;
      }
      console.log(`worker ${port} registered with Conduit`);
      // Re-register on a timer so a `conduit reload` (which clears every
      // dynamic registration, even for an unrelated config change) doesn't
      // silently drop this worker until it next crashes and respawns.
      // Idempotent — see the Admin API section above.
      reregisterTimer = setInterval(() => {
        callAdmin("/upstreams/add", { route: ROUTE, target, weight: 1 }).catch(
          (err) => {
            console.error(
              `worker ${port} periodic re-registration failed: ${err.message}`,
            );
          },
        );
      }, 30_000);
    }
  });

  worker.on("exit", async (code) => {
    if (reregisterTimer) clearInterval(reregisterTimer);
    await callAdmin("/upstreams/remove", { route: ROUTE, target }).catch(
      () => {},
    );
    console.log(`worker ${port} exited (code ${code}), respawning...`);
    setTimeout(() => spawnWorker(port), 500);
  });
}

for (let i = 0; i < NUM_WORKERS; i++) spawnWorker(BASE_PORT + i);
```

```js
// worker.js — your application, one instance per worker process
const http = require("http");
const port = process.env.PORT;

http
  .createServer((req, res) => {
    // req.headers['x-request-id'] is already set by Conduit's XRequestIdGuard —
    // propagate it into your own logs for cross-system correlation.
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(
      JSON.stringify({
        handledBy: `worker-${port}`,
        requestId: req.headers["x-request-id"],
      }),
    );
  })
  .listen(port, "127.0.0.1", () => process.send("ready"));
```

```yaml
# conduit.yaml — the `global:`/`sites:` shape is required here: `global.admin`
# is only recognized on the full config shape, not the flat single-site
# shorthand (`{ "port": 8080, "proxy": {...} }`) also shown elsewhere in this
# repo's docs — the Admin API silently doesn't start under the flat shorthand.
global:
  admin:
    bind: "127.0.0.1:2019"
sites:
  - port: 8080
    proxy:
      "/api":
        strategy:
          round-robin # least-conn/other strategies work too; round-robin
          # makes distribution obvious when trying this out —
          # near-instant responses can make least-conn's tie-
          # breaking consistently favor one worker
        targets:
          - http://127.0.0.1:4001 # worker 0 — a static seed target (Conduit
            # rejects an empty targets list); the rest
            # are added dynamically by pool.js
```

## Python example

The same pattern, using `multiprocessing` and `urllib`/`requests` for the
Admin API calls instead of `child_process`/`http`. A production setup would
more likely put Gunicorn/uvicorn workers behind this instead of hand-rolling
`multiprocessing` — the supervisor's job (register/deregister via the Admin
API) stays the same regardless of what actually manages the worker
processes underneath it.

```python
# pool.py — worker-pool supervisor
import json
import multiprocessing
import os
import time
import urllib.request

NUM_WORKERS = 4
BASE_PORT = 5001
ADMIN_URL = "http://127.0.0.1:2019"
ADMIN_TOKEN = os.environ.get("CONDUIT_ADMIN_TOKEN")  # unset if global.admin.token isn't configured
ROUTE = "/api"
# No `site` field below — this example is single-site, so the registration
# applies to whichever site serves ROUTE. Add site="host:port" for a
# multi-site deployment.


def call_admin(path: str, body: dict) -> dict:
    data = json.dumps(body).encode()
    headers = {"Content-Type": "application/json"}
    if ADMIN_TOKEN:
        headers["Authorization"] = f"Bearer {ADMIN_TOKEN}"
    req = urllib.request.Request(ADMIN_URL + path, data=data, headers=headers, method="POST")
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def run_worker(port: int, ready: multiprocessing.synchronize.Event) -> None:
    from worker import serve  # your application's entry point

    serve(port, ready)


def register_with_retry(target: str, attempts: int = 5, delay_secs: float = 1.0) -> bool:
    """Register `target` and retry a few times on failure (Admin API
    momentarily unreachable, etc.) rather than leaving a live worker
    silently unregistered. The periodic re-register below is the
    longer-term backstop -- this is just for the very first attempt."""
    for i in range(1, attempts + 1):
        try:
            call_admin("/upstreams/add", {"route": ROUTE, "target": target, "weight": 1})
            return True
        except Exception as e:  # noqa: BLE001 - starter code, log and retry
            print(f"worker {target} registration attempt {i}/{attempts} failed: {e}", flush=True)
            if i < attempts:
                time.sleep(delay_secs)
    return False


def supervise(port: int) -> None:
    while True:
        ready = multiprocessing.Event()
        proc = multiprocessing.Process(target=run_worker, args=(port, ready))
        proc.start()
        # Blocks until the worker has actually bound its socket — without this,
        # /upstreams/add could register a port Conduit can route to before
        # anything is listening on it. Note the tradeoff: ready.wait() has no
        # timeout here, so a worker that fails *before* HTTPServer(...) ever
        # succeeds (import error, port already in use, permission error) hangs
        # this one slot forever instead of respawning — the other workers keep
        # running unaffected. Add ready.wait(timeout=...) plus a log line and
        # explicit proc.terminate() on timeout if that failure mode matters for
        # your use case.
        ready.wait()
        target = f"http://127.0.0.1:{port}"
        if not register_with_retry(target):
            print(f"worker {port}: giving up on registration, killing and respawning", flush=True)
            proc.terminate()
            proc.join()
            time.sleep(0.5)
            continue
        print(f"worker {port} registered with Conduit", flush=True)

        # Re-register on a timer so a `conduit reload` (which clears every
        # dynamic registration, even for an unrelated config change) doesn't
        # silently drop this worker until it next crashes and respawns.
        # Idempotent -- see the Admin API section above.
        while proc.is_alive():
            proc.join(timeout=30)
            if proc.is_alive():
                try:
                    call_admin("/upstreams/add", {"route": ROUTE, "target": target, "weight": 1})
                except Exception as e:  # noqa: BLE001 - starter code, log and keep going
                    print(f"worker {port} periodic re-registration failed: {e}", flush=True)

        call_admin("/upstreams/remove", {"route": ROUTE, "target": target})
        print(f"worker {port} exited, respawning...", flush=True)
        time.sleep(0.5)


if __name__ == "__main__":
    procs = [
        multiprocessing.Process(target=supervise, args=(BASE_PORT + i,))
        for i in range(NUM_WORKERS)
    ]
    for p in procs:
        p.start()
    for p in procs:
        p.join()
```

```python
# worker.py — your application, one instance per worker process
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import multiprocessing


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        # self.headers["X-Request-ID"] is already set by Conduit's
        # XRequestIdGuard — propagate it into your own logs.
        body = json.dumps({
            "handledBy": f"worker-{self.server.server_port}",
            "requestId": self.headers.get("X-Request-ID"),
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)


def serve(port: int, ready: multiprocessing.synchronize.Event | None = None) -> None:
    server = HTTPServer(("127.0.0.1", port), Handler)  # binds + listens synchronously
    if ready is not None:
        ready.set()  # signal the supervisor only after the socket is actually listening
    server.serve_forever()
```

```yaml
# conduit.yaml — see the Node.js example above for why the global:/sites:
# shape is required here rather than the flat single-site shorthand.
global:
  admin:
    bind: "127.0.0.1:2019"
sites:
  - port: 8080
    proxy:
      "/api":
        strategy: round-robin
        targets:
          - http://127.0.0.1:5001 # worker 0 — static seed target
```

## What this deliberately does not do

- No custom transport — plain loopback HTTP, reusing everything Conduit
  already has. A bespoke protocol (Unix socket, shared memory) would only
  earn its complexity if it measurably beat this, and nothing here has
  needed that yet.
- Conduit itself does not spawn, supervise, or restart worker processes —
  that responsibility stays entirely in the supervisor script above, kept
  deliberately outside Conduit's own binary/feature-flag system. This is a
  scope boundary, not an oversight: see #290/#291 for the reasoning (a
  Conduit-owned process-supervision subsystem is a materially bigger
  commitment — crash isolation, cross-process debugging, log/trace
  correlation, resource limits — that changes what "Conduit is a reverse
  proxy with zero runtime dependencies" means, and hasn't been taken on).
- No sandboxing of worker code — a worker process is trusted, co-located
  application code, at the same trust level as any other upstream process on
  the host. This is a different trust model from Conduit's in-process WASM/
  Rhai middleware, which run sandboxed inside Conduit's own process (see
  [wasm.md](wasm.md), [rhai.md](rhai.md)).

## Open questions before this becomes a published package

Tracked in [#290](https://github.com/lopatnov/conduit/issues/290) /
[#291](https://github.com/lopatnov/conduit/issues/291): the package name
(placeholder only, not decided), whether to standardize on `child_process`/
`multiprocessing` or delegate to existing tools (`cluster`/PM2 for Node,
Gunicorn/uWSGI for Python) for the actual process management, and how much
of the health-check/backoff logic above is worth generalizing into a real
library versus leaving as copy-paste-and-adapt starter code.
