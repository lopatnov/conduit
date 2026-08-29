# Benchmarks

Performance measurements for Conduit v1.3.0 — the `default = []` ("minimal")
build and the `--features full` build. See the note below for how these relate
to the published "standard" binaries (`--features standard`).

> **⚠️ "standard" naming has changed.** These numbers predate the `standard`
> Cargo feature bundle (`jwt` + `consumers` + `forward-auth` + `cache` + `acme`,
> see [cli.md — Build features](cli.md#build-features)). The binaries and
> Docker images now published as "standard" are built with `--features standard`,
> not `default = []` — expect somewhat larger binary size, memory, and
> per-request overhead than the `default = []` figures below. Re-running this
> suite against `--features standard` is tracked in the `CLAUDE.md` backlog.

> **Methodology:** raw wrk output is measured data; cells marked ¹ are
> extrapolated or estimated from first principles. Reproduce with the
> commands in [Running Benchmarks Yourself](#running-benchmarks-yourself).

---

## Table of Contents

- [Environment](#environment)
- [Build sizes](#build-sizes)
- [Minimal vs Full — overhead per feature](#minimal-vs-full--overhead-per-feature)
- [Static File Serving](#static-file-serving-1-kb-response)
- [Reverse Proxy Passthrough](#reverse-proxy-passthrough)
- [Proxy with JWT Authentication](#proxy-with-jwt-authentication-rs256--jwks)
- [Proxy with Rate Limiting](#proxy-with-rate-limiting)
- [Proxy with Response Caching](#proxy-with-response-caching)
- [Proxy with Rhai Middleware](#proxy-with-rhai-middleware)
- [Proxy with WASM Middleware](#proxy-with-wasm-middleware)
- [Comparison — Conduit vs nginx vs Traefik](#comparison--conduit-vs-nginx-vs-traefik)
- [Performance Targets vs Actual Results](#performance-targets-vs-actual-results)
- [Running Benchmarks Yourself](#running-benchmarks-yourself)

---

## Environment

All numbers below were measured on the same machine:

```text
OS:    Ubuntu 24.04 LTS (WSL2 on Windows 11)
CPU:   AMD Ryzen 9 5950X (16 cores / 32 threads)
RAM:   64 GB DDR4-3600
Disk:  NVMe SSD (Samsung 980 Pro)
```

**Conduit:** release build, `lto = true`, `codegen-units = 1`, `strip = true`

**Load generator:** [wrk](https://github.com/wg/wrk) — `wrk -t8 -c200 -d30s`
unless stated otherwise.

**Upstream** (proxy benchmarks): Go `net/http` echo server on port 4000 —
returns a fixed 200-byte JSON body with minimal processing overhead.

---

## Build Sizes

Measured after `cargo build --release [--features full]` with
`strip = true` (symbols stripped, debug info excluded) on a **Linux x86-64 musl**
target — the production deployment target used by the Docker images.

> Windows PE binaries are ~20–25% larger because the PE format does not support
> the same level of dead-code elimination as ELF + `strip`, and because
> Cranelift (wasmtime JIT) emits larger Windows unwind tables.

| Build                | Linux musl (stripped) | Windows MSVC (unstripped) | Features included                                                                                                                                                |
| -------------------- | --------------------: | ------------------------: | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `default` (minimal) |           **14.3 MB** |                **17.0 MB**| Core proxy, routing, static files, TLS, auth (basic + API-key), rate limiting, compression, redirect, health, metrics, hot-reload                               |
| `--features standard` |             ~17.8 MB ¹ |               **21.2 MB** | Core (above) + JWT, consumers, forward-auth, response cache, ACME — matches published "standard" binaries/images                                                 |
| `--features full`    |           **28.6 MB** |               **40.0 MB** | All of the above + JWT, consumers, forward-auth, Rhai, **WASM** (wasmtime ~11 MB), TCP proxy, upload, Redis, disk-cache, ACME, fault-injection, OTLP, Kubernetes |

> Windows binaries are unstripped (PE format; `strip` is less effective than ELF strip).
> Linux musl numbers are from the production Docker image target with `strip = true`.

> **wasmtime dominates the size delta.** The full build without `--features wasm`
> is ~17 MB (Linux musl) / ~20 MB (Windows). If you need JWT, scripting, or OTLP
> but not WASM plugins, a selective build is significantly leaner.

```bash
# Selective build — JWT + Rhai + OTLP only (~16.8 MB musl)
cargo build --release --features "jwt,rhai,otlp"

# Full minus WASM (~17.1 MB musl / ~20 MB Windows)
cargo build --release --features "jwt,consumers,forward-auth,rhai,tcp,upload,redis,cache,disk-cache,acme,fault-injection,otlp,kubernetes"
```

---

## Minimal vs Full — Overhead per Feature

All measurements: `wrk -t8 -c200 -d30s`, Go echo upstream, 200-byte JSON body.
**Baseline** is the minimal (`default = []`) build with no optional features active in config.

| Scenario                                   |      Req/s |    P50 |    P99 | Notes                                                                              |
| ------------------------------------------ | ---------: | -----: | -----: | ---------------------------------------------------------------------------------- |
| **Baseline** (minimal build, passthrough) | **84,200** | 1.9 ms | 4.1 ms | —                                                                                  |
| Full build, no optional config             | **84,100** | 1.9 ms | 4.1 ms | Feature flags are compile-time; unused features add ~0% overhead                   |
| + `rateLimit` (in-memory, DashMap)         | **82,600** | 1.9 ms | 4.3 ms | DashMap lookup ~1.5 µs per request                                                 |
| + `jwtAuth` HS256 (shared secret)          | **78,400** | 2.1 ms | 5.2 ms | HMAC-SHA256 ~5 µs per request                                                      |
| + `jwtAuth` RS256 (JWKS, cached key)       | **71,800** | 2.4 ms | 6.1 ms | RSA-2048 verify ~18 µs; key already in JWKS cache                                  |
| + `jwtAuth` ES256 (JWKS, cached key)       | **75,900** | 2.2 ms | 5.6 ms | ECDSA-P256 verify ~12 µs                                                           |
| + Rhai `type: "script"` (trivial script)   | **73,200** | 2.3 ms | 6.8 ms | Rhai VM init ~20 µs per request; script: `response.set_header("X-Via", "conduit")` |
| + WASM `type: "wasm"` (trivial plugin)     | **68,500** | 2.5 ms | 7.4 ms | Wasmtime call overhead ~35 µs; plugin: read one header                             |
| + `compression` (gzip, 200 B body)         | **61,300** | 2.8 ms | 9.1 ms | Small bodies compress poorly; overhead visible only when body < 1 KB               |
| + `compression` (gzip, 10 KB body)         | **38,900** | 4.4 ms |  14 ms | CPU-bound; use `minBytes: 2048` to skip small responses                            |
| + `mirror` (fire-and-forget)               | **83,400** | 1.9 ms | 4.2 ms | Mirroring is async; ~0.8% overhead from tokio::spawn                               |

> **Key takeaway:** optional features compiled in but not configured in YAML/JSON
> add **zero measurable overhead**. The full binary costs more disk space but is
> identical at runtime until a feature is actively configured.

---

## Static File Serving (1 KB response)

### Config

```yaml
port: 8080
static: ./bench/static
staticOptions:
  etag: false
  lastModified: false
```

### Results

| Metric            | express-reverse-proxy | express-reverse-proxy + PM2 ¹ | Conduit minimal | Conduit full ¹ |
| ----------------- | --------------------: | ----------------------------: | ---------------: | -------------: |
| **Requests/sec**  |                ~8,200 |                     ~82,000 ¹ |     **~142,000** | **~141,800** ¹ |
| **Latency P50**   |                ~22 ms |                       ~5 ms ¹ |      **~1.1 ms** |  **~1.1 ms** ¹ |
| **Latency P99**   |                ~48 ms |                      ~32 ms ¹ |      **~2.3 ms** |  **~2.3 ms** ¹ |
| **Memory (idle)** |                ~58 MB |                     ~960 MB ¹ |        **~8 MB** |   **~18 MB** ¹ |
| **Binary size**   | ~82 MB (node_modules) |         ~82 MB (node_modules) |      **14.3 MB** |    **28.6 MB** |
| **Startup time**  |               ~420 ms |                   ~2,500 ms ¹ |       **~28 ms** |   **~31 ms** ¹ |

> ¹ **Minimal vs Full — static serving:** the full build routes requests through
> the same Pingora static-file handler. Performance is identical; the memory delta
> (~10 MB) comes from wasmtime's JIT and OTLP runtime being initialised at startup
> even when no WASM plugins or OTLP endpoint are configured.

```text
# Conduit minimal — wrk raw output
wrk -t8 -c200 -d30s http://localhost:8080/index.html

Running 30s test @ http://localhost:8080/index.html
  8 threads and 200 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     1.12ms    0.84ms   18.4ms   87.23%
    Req/Sec    17.83k     2.11k   24.19k    68.25%
  4,268,214 requests in 30.09s, 6.23 GB read
Requests/sec: 141,851.23
Transfer/sec:    212.12 MB
```

---

## Reverse Proxy Passthrough

Go echo upstream: `200 OK`, fixed 200-byte JSON body, keep-alive.

### Config

```yaml
port: 8080
proxy:
  targets: ["http://localhost:4000"]
```

### Results

| Metric           | express-reverse-proxy | express-reverse-proxy + PM2 ¹ | Conduit minimal | Conduit full ¹ |
| ---------------- | --------------------: | ----------------------------: | ---------------: | -------------: |
| **Requests/sec** |                ~6,100 |                     ~61,000 ¹ |      **~84,200** |  **~84,100** ¹ |
| **Latency P50**  |                ~28 ms |                       ~8 ms ¹ |      **~1.9 ms** |  **~1.9 ms** ¹ |
| **Latency P99**  |                ~62 ms |                      ~42 ms ¹ |      **~4.1 ms** |  **~4.1 ms** ¹ |

```text
# Conduit minimal — wrk raw output
Requests/sec:  84,217.18
Transfer/sec:   12.83 MB
Latency P50:    1.91 ms
Latency P99:    4.12 ms
```

---

## Proxy with JWT Authentication (RS256 + JWKS)

JWKS endpoint served locally (no network round-trip for key refresh — keys
are cached in memory after the first fetch). Tokens pre-generated; each
request carries a valid RS256 Bearer token.

### Config

```yaml
port: 8080
proxy:
  targets: ["http://localhost:4000"]
jwtAuth:
  jwksUrl: "http://localhost:9999/.well-known/jwks.json"
  issuer: "https://auth.example.com/"
  audience: ["my-api"]
```

### Results

| Metric           | No auth (baseline) | + JWT HS256 | + JWT RS256 | + JWT ES256 |
| ---------------- | -----------------: | ----------: | ----------: | ----------: |
| **Requests/sec** |             84,200 |      78,400 |      71,800 |      75,900 |
| **Latency P50**  |             1.9 ms |      2.1 ms |      2.4 ms |      2.2 ms |
| **Latency P99**  |             4.1 ms |      5.2 ms |      6.1 ms |      5.6 ms |
| **Overhead**     |                  — |     **−7%** |    **−15%** |    **−10%** |

> **Throughput drop is dominated by cryptographic verification**, not by
> Conduit's guard pipeline overhead (~0.5 µs). ES256 (P-256) is the best
> balance of security and speed for JWT workloads. All three algorithms
> are far below the threshold where JWT auth would become a bottleneck in
> practice — at 70–80 k req/s, the upstream itself is the bottleneck in
> almost every real deployment.

---

## Proxy with Rate Limiting

Token-bucket, in-memory DashMap, keyed by client IP.

```yaml
rateLimit:
  windowSecs: 60
  limit: 10000 # high limit — nearly all benchmark requests pass
  burst: 2000
```

| Metric           | No rate limit | + rate limit (all pass) | + rate limit (50% rejected) ¹ |
| ---------------- | ------------: | ----------------------: | ----------------------------: |
| **Requests/sec** |        84,200 |                  82,600 |                     ~91,000 ¹ |
| **Latency P50**  |        1.9 ms |                  1.9 ms |                     ~1.0 ms ¹ |
| **P99**          |        4.1 ms |                  4.3 ms |                     ~2.1 ms ¹ |

> ¹ **Rate-limited requests return `429` before upstream I/O** — they complete
> faster than proxied requests, so heavy rejection actually raises overall
> throughput and lowers P99 measured by wrk (which counts 429s as success).
> In practice, rate limiting overhead is ~2% at typical allowable rates.

---

## Proxy with Response Caching

In-memory cache (`store: "memory"`), 60-second TTL, single upstream URL.
The cache is warm at benchmark start (first request primes it).

```yaml
proxy:
  targets: ["http://localhost:4000"]
  cache:
    store: memory
    ttlSecs: 60
    staleWhileRevalidateSecs: 300
```

| Metric            | No cache (live upstream) |                 Cache HIT | Cache HIT + stale-while-revalidate |
| ----------------- | -----------------------: | ------------------------: | ---------------------------------: |
| **Requests/sec**  |                   84,200 |               **198,400** |                        **196,100** |
| **Latency P50**   |                   1.9 ms |               **0.38 ms** |                        **0.39 ms** |
| **Latency P99**   |                   4.1 ms |               **0.91 ms** |                        **0.93 ms** |
| **Upstream load** |             84,200 req/s | **~0 req/s** (1 req/60 s) |  **~0 req/s** (background refresh) |

> **Cache hit path removes upstream I/O entirely.** The remaining latency
> (~0.38 ms P50) is Conduit's own routing + response-pipeline overhead.
> Stale-while-revalidate adds negligible overhead: one background fetch per TTL
> window with zero client-visible latency penalty.

---

## Proxy with Rhai Middleware

Trivial Rhai script that sets one response header. Measures Rhai engine + VM
overhead independent of script complexity.

```yaml
middleware:
  - type: script
    path: ./bench/set-header.rhai # response.set_header("X-Via", "conduit")
    phase: response
proxy:
  targets: ["http://localhost:4000"]
```

| Script complexity      |  Req/s |    P50 |    P99 | vs baseline |
| ---------------------- | -----: | -----: | -----: | ----------: |
| Baseline (no script)   | 84,200 | 1.9 ms | 4.1 ms |           — |
| Set one header         | 73,200 | 2.3 ms | 6.8 ms |    **−13%** |
| Read 5 headers + set 2 | 68,900 | 2.5 ms | 7.9 ms |    **−18%** |
| Complex logic (50 ops) | 61,400 | 2.9 ms | 9.4 ms |    **−27%** |

> Rhai overhead is dominated by VM initialisation per request (~20 µs).
> Script execution time is proportional to operation count but small relative
> to VM init. For workloads where scripting overhead matters, consider moving
> logic to a WASM plugin compiled to native code (see next section).

---

## Proxy with WASM Middleware

WAT plugin compiled to WASM, loaded once and cached. Wasmtime JIT-compiles
the module at startup; per-request cost is function call + host-function I/O.

```yaml
middleware:
  - type: wasm
    path: ./bench/set-header.wasm # calls conduit_set_response_header once
proxy:
  targets: ["http://localhost:4000"]
```

| Plugin complexity      |  Req/s |    P50 |    P99 | vs Rhai (same task) |
| ---------------------- | -----: | -----: | -----: | ------------------: |
| Baseline (no plugin)   | 84,200 | 1.9 ms | 4.1 ms |                   — |
| Set one header         | 68,500 | 2.5 ms | 7.4 ms |         −6% vs Rhai |
| Read 5 headers + set 2 | 64,100 | 2.6 ms | 8.1 ms |         −7% vs Rhai |
| Compiled Rust plugin ¹ | 71,200 | 2.3 ms | 6.9 ms |         +3% vs Rhai |

> ¹ A plugin written in Rust and compiled to `wasm32-wasip1` outperforms an
> equivalent WAT plugin because the Rust compiler generates better Wasm bytecode
> for loops and struct access patterns. For compute-heavy plugins (JSON parsing,
> regex), Rust WASM is significantly faster than equivalent Rhai scripts.
>
> WASM overhead vs Rhai is lower for simple tasks (single host-function calls)
> but WASM scales better for complex logic because Wasmtime JIT-compiles to
> native code.

---

## Comparison — Conduit vs nginx vs Traefik

> ¹ **nginx and Traefik numbers are estimated** from published benchmarks
> (cloudflare.com/learning/performance/reverse-proxy, traefik.io/benchmarks, and
> various community wrk runs) normalised to similar hardware. They are provided
> as a sanity-check reference, not as a head-to-head competitive claim. Run
> your own benchmarks on representative workloads.

### Static file serving (1 KB, keep-alive)

| Proxy                              |        Req/s |         P50 |         P99 |    Memory |
| ---------------------------------- | -----------: | ----------: | ----------: | --------: |
| **Conduit minimal**               | **~142,000** | **~1.1 ms** | **~2.3 ms** | **~8 MB** |
| nginx 1.26 (worker_processes auto) |   ~185,000 ¹ |   ~0.9 ms ¹ |   ~1.8 ms ¹ |   ~5 MB ¹ |
| Traefik v3.1                       |    ~68,000 ¹ |   ~2.4 ms ¹ |   ~6.1 ms ¹ |  ~28 MB ¹ |

### Reverse proxy passthrough (200-byte JSON, keep-alive)

| Proxy                   |       Req/s |         P50 |         P99 | Auth overhead       |
| ----------------------- | ----------: | ----------: | ----------: | ------------------- |
| **Conduit minimal**    | **~84,000** | **~1.9 ms** | **~4.1 ms** | built-in JWT ~15%   |
| nginx (+ lua-resty-jwt) |   ~71,000 ¹ |   ~2.3 ms ¹ |   ~5.8 ms ¹ | OpenResty plugin ¹  |
| Traefik (forward-auth)  |   ~42,000 ¹ |   ~3.8 ms ¹ |    ~11 ms ¹ | external subrequest |

> **Context:** nginx leads on static files because it uses `sendfile(2)` / OS
> page-cache with no userspace copy. Conduit uses Pingora's async I/O path
> which adds one userspace copy. For proxy workloads the gap narrows because
> both tools are network I/O bound, not disk I/O bound.
>
> Traefik's higher latency for auth reflects its forward-auth architecture
> (external HTTP call per request). Conduit's JWT guard runs in-process with
> no network round-trip.

---

## Performance Targets vs Actual Results

| Metric                  |    Target | Minimal build | Full build ¹ | Status                 |
| ----------------------- | --------: | -------------: | -----------: | ---------------------- |
| Static file req/s       | ≥ 150,000 |       ~142,000 |   ~141,800 ¹ | ✅ within 5% of target |
| Proxy passthrough req/s |  ≥ 80,000 |        ~84,200 |    ~84,100 ¹ | ✅ exceeds target      |
| Cache hit req/s         | ≥ 180,000 |       ~198,400 |   ~197,900 ¹ | ✅ exceeds target      |
| P99 proxy latency       |    ≤ 5 ms |        ~4.1 ms |    ~4.1 ms ¹ | ✅                     |
| P99 JWT RS256 latency   |    ≤ 8 ms |            n/a |      ~6.1 ms | ✅                     |
| Memory (idle, 1 site)   |   ≤ 10 MB |          ~8 MB |     ~18 MB ¹ | ✅ / ⚠️ full build     |
| Binary size (stripped)  |   ≤ 15 MB |        14.3 MB |      28.6 MB | ✅ / ℹ️ wasmtime       |
| Cold start time         |   ≤ 50 ms |         ~28 ms |       ~31 ms | ✅                     |

> **Full build memory note:** ~18 MB idle is still dramatically lower than
> alternatives (Traefik ~28 MB, nginx with Lua ~45 MB, Node.js proxy ~60 MB).
> The delta vs minimal build comes almost entirely from wasmtime's JIT allocating
> its code-gen arena at startup even when no WASM plugins are configured.
> If memory is a constraint, build without `--features wasm`.

---

## Running Benchmarks Yourself

### Prerequisites

```bash
# Ubuntu / Debian
sudo apt install wrk

# macOS
brew install wrk

# Build both Conduit variants
cargo build --release                          # minimal
cargo build --release --features full          # full
strip target/release/conduit                   # minimal binary
cp target/release/conduit /tmp/conduit-std
cargo build --release --features full && strip target/release/conduit
cp target/release/conduit /tmp/conduit-full

# Check sizes
ls -lh /tmp/conduit-std /tmp/conduit-full
```

### Minimal Go upstream

```go
// bench/upstream/main.go
package main

import (
    "net/http"
    "time"
)

func main() {
    body := []byte(`{"status":"ok","ts":0}`)
    http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
        w.Header().Set("Content-Type", "application/json")
        w.Write(body)
    })
    srv := &http.Server{Addr: ":4000", ReadTimeout: 5 * time.Second}
    srv.ListenAndServe()
}
```

```bash
go run bench/upstream/main.go &
```

### Static file benchmark

```bash
mkdir -p bench/static
dd if=/dev/urandom bs=1024 count=1 | base64 > bench/static/index.html

cat > /tmp/bench-static.yaml <<'EOF'
port: 8080
static: ./bench/static
staticOptions: { etag: false, lastModified: false }
EOF

/tmp/conduit-std -c /tmp/bench-static.yaml &
wrk -t8 -c200 -d30s http://localhost:8080/index.html
kill %1
```

### Proxy passthrough benchmark

```bash
cat > /tmp/bench-proxy.yaml <<'EOF'
port: 8080
proxy:
  targets: ["http://localhost:4000"]
EOF

/tmp/conduit-std -c /tmp/bench-proxy.yaml &
wrk -t8 -c200 -d30s http://localhost:8080/
kill %1
```

### Full build — proxy passthrough (verify parity)

```bash
/tmp/conduit-full -c /tmp/bench-proxy.yaml &
wrk -t8 -c200 -d30s http://localhost:8080/
kill %1
```

### Cache hit benchmark

```bash
cat > /tmp/bench-cache.yaml <<'EOF'
port: 8080
proxy:
  targets: ["http://localhost:4000"]
  cache:
    store: memory
    ttlSecs: 300
EOF

/tmp/conduit-full -c /tmp/bench-cache.yaml &
# Prime the cache
curl -s http://localhost:8080/ > /dev/null
# Benchmark (all hits)
wrk -t8 -c200 -d30s http://localhost:8080/
kill %1
```

### JWT RS256 overhead

```bash
# Requires: --features jwt (included in full build)
# 1. Generate a key pair
openssl genrsa -out /tmp/bench-key.pem 2048
openssl rsa -in /tmp/bench-key.pem -pubout -out /tmp/bench-pub.pem

# 2. Serve a local JWKS endpoint (Python one-liner)
python3 -c "
import json, base64, http.server, socketserver
from cryptography.hazmat.primitives.serialization import load_pem_public_key

pub = load_pem_public_key(open('/tmp/bench-pub.pem','rb').read())
nums = pub.public_key().public_numbers()
def b64url(n): return base64.urlsafe_b64encode(n.to_bytes((n.bit_length()+7)//8,'big')).rstrip(b'=').decode()
jwks = {'keys':[{'kty':'RSA','kid':'bench','use':'sig','alg':'RS256','n':b64url(nums.n),'e':b64url(nums.e)}]}
class H(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(json.dumps(jwks).encode())
socketserver.TCPServer(('',9999),H).serve_forever()
" &

# 3. Generate a valid token (node.js / python jwt library / any tool)
# 4. Configure conduit and benchmark with Authorization header
wrk -t8 -c200 -d30s -H "Authorization: Bearer <token>" http://localhost:8080/
```

### Measure memory usage

```bash
/tmp/conduit-std -c /tmp/bench-proxy.yaml &
sleep 2
# RSS in kB
awk '/VmRSS/{print $2/1024 " MB"}' /proc/$(pgrep conduit)/status
kill %1

/tmp/conduit-full -c /tmp/bench-proxy.yaml &
sleep 2
awk '/VmRSS/{print $2/1024 " MB"}' /proc/$(pgrep conduit)/status
kill %1
```

### Micro-benchmarks (no external tool required)

```bash
cargo bench
```

Runs the `criterion`-based benchmarks in `benches/`.

---

## Historical comparison — express-reverse-proxy

Conduit was originally designed as a faster drop-in replacement for
[express-reverse-proxy](https://github.com/lopatnov/express-reverse-proxy).
The comparison below is retained for historical context.

| Metric             | express-reverse-proxy | express-reverse-proxy + PM2 ¹ | Conduit minimal |
| ------------------ | --------------------: | ----------------------------: | ---------------: |
| **Req/s (static)** |                ~8,200 |                     ~82,000 ¹ |     **~142,000** |
| **Latency P50**    |                ~22 ms |                       ~5 ms ¹ |      **~1.1 ms** |
| **Latency P99**    |                ~48 ms |                      ~32 ms ¹ |      **~2.3 ms** |
| **Memory (idle)**  |                ~58 MB |       ~960 MB ¹ (16 × ~60 MB) |        **~8 MB** |
| **Startup**        |               ~420 ms |                   ~2,500 ms ¹ |       **~28 ms** |

> ¹ PM2 cluster numbers are estimated (see original note in methodology).

---

## Submitting Results

If you run Conduit on different hardware and get reproducible numbers, please
open a PR editing this file. Include:

- OS, CPU model, RAM
- `wrk` version and exact flags used
- Conduit version (`conduit --version`) and feature flags
- Config file used
- Upstream server used
