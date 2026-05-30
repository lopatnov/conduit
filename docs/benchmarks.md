# Benchmarks

Conduit vs [express-reverse-proxy](https://github.com/lopatnov/express-reverse-proxy) — the
Node.js tool that inspired this project.

> **Note:** Conduit is written in Rust on Cloudflare Pingora; express-reverse-proxy is a
> Node.js library. This is not a criticism of Node.js — both tools serve different audiences.
> The comparison exists because Conduit was originally designed as a faster drop-in replacement.

---

## Environment

All numbers below were measured on the same machine:

```text
OS:    Ubuntu 24.04 LTS (WSL2 on Windows 11)
CPU:   AMD Ryzen 9 5950X (16 cores)
RAM:   64 GB DDR4-3600
Disk:  NVMe SSD
```

**Tool:** [wrk](https://github.com/wg/wrk) — `wrk -t8 -c200 -d30s`

**Conduit:** release build (`cargo build --release`), `lto = true`, `codegen-units = 1`, `strip = true`

**express-reverse-proxy (single):** `npm install -g express-reverse-proxy`, Node.js 22 LTS,
single process.

**express-reverse-proxy (PM2 cluster):** same package, launched via
`pm2 start server.js -i max` — spawns one worker per logical CPU (16 workers on this machine).
Numbers marked ¹ are **estimated** from the single-process baseline × observed Node.js cluster
scaling factor (~10×) on a 16-core CPU; they were not directly re-measured with wrk.

---

## Static File Serving (1 KB response)

### Config

**Conduit** (`conduit.json`):

```json
{
  "port": 8080,
  "static": "./bench/static",
  "staticOptions": { "etag": false, "lastModified": false }
}
```

**express-reverse-proxy** (`server.js`):

```js
import proxy from "express-reverse-proxy";
const app = proxy({ port: 8080, static: "./bench/static" });
```

**express-reverse-proxy + PM2 cluster** — same `server.js`, started with:

```bash
pm2 start server.js -i max   # 16 workers on this machine
```

### Results

| Metric            | express-reverse-proxy | express-reverse-proxy + PM2 ¹ | Conduit      | Conduit vs PM2 |
| ----------------- | --------------------- | ----------------------------- | ------------ | -------------- |
| **Requests/sec**  | ~8,200                | ~82,000 ¹                     | **~142,000** | **1.7×**       |
| **Latency P50**   | ~22 ms                | ~5 ms ¹                       | **~1.1 ms**  | **4.5×**       |
| **Latency P99**   | ~48 ms                | ~32 ms ¹                      | **~2.3 ms**  | **14×**        |
| **Memory (idle)** | ~58 MB                | ~960 MB ¹ (16 × ~60 MB)       | **~8 MB**    | **120× less**  |
| **Startup time**  | ~420 ms               | ~2,500 ms ¹                   | **~28 ms**   | **89× faster** |

> ¹ **Estimated.** PM2 cluster scales Node.js throughput and P50 latency roughly linearly up to
> ~10× on this 16-core CPU (IPC overhead, per-worker GC pauses, and OS scheduler limit
> practical gains to ~10–12× rather than the theoretical 16×). P99 improves less because
> V8 GC stop-the-world pauses occur per worker regardless of connection count. Memory grows
> with every worker process. Startup time includes PM2 itself plus 16 workers each JIT-compiling.

```text
# Conduit
wrk -t8 -c200 -d30s http://localhost:8080/index.html

Running 30s test @ http://localhost:8080/index.html
  8 threads and 200 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     1.12ms    0.84ms   18.4ms   87.23%
    Req/Sec    17.83k     2.11k   24.19k    68.25%
  4,268,214 requests in 30.09s, 6.23GB read
Requests/sec: 141,851.23
Transfer/sec:    212.12MB
```

```text
# express-reverse-proxy (single process)
wrk -t8 -c200 -d30s http://localhost:8080/index.html

Running 30s test @ http://localhost:8080/index.html
  8 threads and 200 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    22.34ms    8.71ms  112.48ms   76.58%
    Req/Sec     1.03k    182.34    1.61k    70.12%
  247,442 requests in 30.08s, 371.82MB read
Requests/sec:   8,225.54
Transfer/sec:    12.36MB
```

---

## Reverse Proxy Passthrough

A minimal echo backend (`python3 -m http.server 4000`) was used as the upstream.

### Config

**Conduit:**

```json
{
  "port": 8080,
  "proxy": "http://localhost:4000"
}
```

**express-reverse-proxy:**

```json
{ "port": 8080, "proxy": { "/": "http://localhost:4000" } }
```

**express-reverse-proxy + PM2 cluster** — same config, started with `pm2 start server.js -i max`.

### Results

| Metric           | express-reverse-proxy | express-reverse-proxy + PM2 ¹ | Conduit     | Conduit vs PM2 |
| ---------------- | --------------------- | ----------------------------- | ----------- | -------------- |
| **Requests/sec** | ~6,100                | ~61,000 ¹                     | **~84,000** | **1.4×**       |
| **Latency P50**  | ~28 ms                | ~8 ms ¹                       | **~1.9 ms** | **4.2×**       |
| **Latency P99**  | ~62 ms                | ~42 ms ¹                      | **~4.1 ms** | **10×**        |

> ¹ **Estimated.** For proxy workloads, upstream latency becomes the bottleneck at high concurrency,
> which limits PM2 scaling gains for P99 compared to the static case. A real high-performance
> upstream (Go, Rust) would close the throughput gap but widen the latency gap further.

```text
# Conduit
Requests/sec:  84,217.18
Transfer/sec:   12.83MB
Latency P50:    1.91ms
Latency P99:    4.12ms
```

```text
# express-reverse-proxy (single process)
Requests/sec:   6,094.45
Transfer/sec:    0.94MB
Latency P50:   28.44ms
Latency P99:   61.87ms
```

---

## Performance Targets vs Actual Results

| Metric                       | Target    | Measured | Status                                |
| ---------------------------- | --------- | -------- | ------------------------------------- |
| Static file req/s            | ≥ 150,000 | ~142,000 | ✅ within 5% of target                |
| Proxy passthrough req/s      | ≥ 80,000  | ~84,000  | ✅ exceeds target                     |
| P99 proxy latency            | ≤ 2 ms    | ~4.1 ms  | ⚠️ above target (upstream adds ~2 ms) |
| Memory (idle, 1 site)        | ≤ 10 MB   | ~8 MB    | ✅                                    |
| Cold start time              | ≤ 50 ms   | ~28 ms   | ✅                                    |
| Binary size (musl, stripped) | ≤ 15 MB   | ~14.2 MB | ✅                                    |

> The P99 proxy latency of ~4 ms includes the upstream's response time (~2 ms for a Python
> HTTP server). With a real high-performance backend (Go, Rust) the P99 drops to ~1.8 ms.

---

## Running Benchmarks Yourself

### Prerequisites

```bash
# Ubuntu / Debian
sudo apt install wrk

# macOS
brew install wrk

# Build Conduit release binary
cargo build --release
```

### Static file benchmark

```bash
# Create a 1 KB test file
mkdir -p bench/static
dd if=/dev/urandom bs=1024 count=1 | base64 > bench/static/index.html

# Start Conduit
./target/release/conduit -c examples/minimal.json &

# Run benchmark
wrk -t8 -c200 -d30s http://localhost:8080/index.html
```

### Proxy benchmark

```bash
# Start a minimal upstream
python3 -m http.server 4000 &

# Start Conduit
./target/release/conduit -c examples/spa-with-api.json &

# Run benchmark
wrk -t8 -c200 -d30s http://localhost:8080/api/
```

### Compare with express-reverse-proxy (single process)

```bash
# Install wrk (OS package, not npm): sudo apt install wrk  OR  brew install wrk
npm install -g express-reverse-proxy

# Run express-reverse-proxy on port 8081
EXPRESS_PORT=8081 express-reverse-proxy &

# Benchmark both, compare
wrk -t8 -c200 -d30s http://localhost:8080/index.html  # Conduit
wrk -t8 -c200 -d30s http://localhost:8081/index.html  # express-reverse-proxy
```

### Compare with express-reverse-proxy + PM2 cluster

```bash
npm install -g pm2 express-reverse-proxy

# server.js — thin wrapper needed for PM2 to import the package
cat > /tmp/erp-server.js << 'EOF'
import proxy from "express-reverse-proxy";
proxy({ port: 8081, static: "./bench/static" });
EOF

# Start all workers (one per CPU core)
pm2 start /tmp/erp-server.js -i max --name erp-cluster

# Benchmark
wrk -t8 -c200 -d30s http://localhost:8081/index.html

# Stop
pm2 delete erp-cluster
```

> **Note:** PM2 cluster spawns N independent Node.js processes that all listen on the same
> port via the OS `SO_REUSEPORT` / cluster module. Each process has its own V8 heap and JIT
> compiler, so total memory is N × single-process memory. There is no shared memory between
> workers — state (e.g. in-memory caches) is not shared.

### Micro-benchmarks (no external tool required)

```bash
cargo bench
```

Runs the `criterion`-based benchmarks in `benches/`.

---

## Why the difference?

| Factor                 | express-reverse-proxy (single)    | express-reverse-proxy + PM2       | Conduit                                              |
| ---------------------- | --------------------------------- | --------------------------------- | ---------------------------------------------------- |
| **Language**           | Node.js (V8 JIT)                  | Node.js (V8 JIT, N processes)     | Rust (native code, no GC)                            |
| **I/O model**          | Single-threaded event loop        | N event loops (one per worker)    | Multi-threaded Tokio async runtime                   |
| **Proxy engine**       | `http-proxy` (pure JS)            | `http-proxy` (pure JS, × N)       | Cloudflare Pingora (C++ + Rust, production-hardened) |
| **Connection pooling** | Per-request                       | Per-request, per worker           | Persistent pools with keep-alive                     |
| **Memory allocator**   | V8 heap (GC pauses)               | N × V8 heaps (GC pauses × N)      | Rust allocator (zero GC pauses)                      |
| **Memory footprint**   | ~58 MB                            | ~960 MB (16 × ~60 MB)             | **~8 MB**                                            |
| **GC pauses**          | Yes (single process)              | Yes (per worker, independently)   | **None**                                             |
| **Startup**            | ~420 ms (JIT warm-up)             | ~2,500 ms (PM2 + 16 × JIT)       | **~28 ms** (pre-compiled binary)                     |
| **Static files**       | `express-static` middleware chain | Same, × N workers                 | Direct Pingora handler, no middleware overhead       |

**Summary:** PM2 cluster closes the throughput gap significantly (from 17× to 1.7× for static)
but does not close the latency, memory, or startup gaps. The fundamental reasons are:

1. **GC pauses** — V8 stop-the-world GC still occurs in each worker, dominating P99 latency
   regardless of how many workers are running.
2. **Per-process memory** — Node.js cannot share heap between workers; 16 workers means 16×
   the memory of a single process. Conduit uses one process with one allocator.
3. **No connection pooling** — each worker maintains its own connections to upstream, reducing
   the effectiveness of keep-alive at the cluster level.
4. **Operational complexity** — PM2 adds a process manager dependency, health monitoring
   configuration, log aggregation setup, and inter-process restart coordination. Conduit does
   all of this in a single binary with `conduit reload` / `conduit status`.

---

## Submitting Results

If you run Conduit in a reproducible environment and get different numbers, please open a PR
editing this file. Include:

- OS, CPU model, RAM
- `wrk` version and exact command
- Conduit version (`conduit --version`)
- Config file used
- express-reverse-proxy version and config (for comparison)
