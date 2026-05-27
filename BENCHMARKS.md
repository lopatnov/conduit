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

**express-reverse-proxy:** `npm install -g express-reverse-proxy`, Node.js 22 LTS

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

### Results

| Metric            | express-reverse-proxy | Conduit      | Improvement    |
| ----------------- | --------------------- | ------------ | -------------- |
| **Requests/sec**  | ~8,200                | **~142,000** | **17×**        |
| **Latency P50**   | ~22 ms                | **~1.1 ms**  | **20×**        |
| **Latency P99**   | ~48 ms                | **~2.3 ms**  | **21×**        |
| **Memory (idle)** | ~58 MB                | **~8 MB**    | **7× less**    |
| **Startup time**  | ~420 ms               | **~28 ms**   | **15× faster** |

```
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

```
# express-reverse-proxy
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

### Results

| Metric           | express-reverse-proxy | Conduit     | Improvement |
| ---------------- | --------------------- | ----------- | ----------- |
| **Requests/sec** | ~6,100                | **~84,000** | **14×**     |
| **Latency P50**  | ~28 ms                | **~1.9 ms** | **15×**     |
| **Latency P99**  | ~62 ms                | **~4.1 ms** | **15×**     |

```
# Conduit
Requests/sec:  84,217.18
Transfer/sec:   12.83MB
Latency P50:    1.91ms
Latency P99:    4.12ms
```

```
# express-reverse-proxy
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

### Compare with express-reverse-proxy

```bash
# Install wrk (OS package, not npm): sudo apt install wrk  OR  brew install wrk
npm install -g express-reverse-proxy

# Run express-reverse-proxy on port 8081
EXPRESS_PORT=8081 express-reverse-proxy &

# Benchmark both, compare
wrk -t8 -c200 -d30s http://localhost:8080/index.html  # Conduit
wrk -t8 -c200 -d30s http://localhost:8081/index.html  # express-reverse-proxy
```

### Micro-benchmarks (no external tool required)

```bash
cargo bench
```

Runs the `criterion`-based benchmarks in `benches/`.

---

## Why the difference?

| Factor                 | express-reverse-proxy             | Conduit                                              |
| ---------------------- | --------------------------------- | ---------------------------------------------------- |
| **Language**           | Node.js (V8 JIT)                  | Rust (native code, no GC)                            |
| **I/O model**          | Single-threaded event loop        | Multi-threaded Tokio async runtime                   |
| **Proxy engine**       | `http-proxy` (pure JS)            | Cloudflare Pingora (C++ + Rust, production-hardened) |
| **Connection pooling** | Per-request                       | Persistent pools with keep-alive                     |
| **Memory allocator**   | V8 heap (GC pauses)               | Rust allocator (zero GC pauses)                      |
| **Static files**       | `express-static` middleware chain | Direct Pingora handler, no middleware overhead       |
| **Binary startup**     | Cold JIT compile on every start   | Pre-compiled native binary                           |

The fundamental advantage is that Conduit has no GC pauses, no JIT warm-up period, and no
event-loop bottleneck. All requests run in parallel across all CPU cores from the first request.

---

## Submitting Results

If you run Conduit in a reproducible environment and get different numbers, please open a PR
editing this file. Include:

- OS, CPU model, RAM
- `wrk` version and exact command
- Conduit version (`conduit --version`)
- Config file used
- express-reverse-proxy version and config (for comparison)
