# Benchmarks

Benchmarks compare Conduit against `express-reverse-proxy` (the Node.js project this was
designed to replace) running identical workloads on the same machine.

## Environment

- **CPU:** AMD EPYC 7763 (2× sockets, 64 cores each)
- **OS:** Ubuntu 22.04 LTS
- **Kernel:** 5.15.0
- **Load generator:** [wrk](https://github.com/wg/wrk) — `wrk -t8 -c200 -d30s`
- **Conduit:** release build, `lto = true`, `codegen-units = 1`, `strip = true`
- **express-reverse-proxy:** Node.js 20, default settings

Both servers ran on the same machine. The upstream for proxy tests was a minimal
`tokio::net::TcpListener` that immediately returned `HTTP/1.1 200 OK\r\n\r\n`.

## Results

### Static file serving (1 KB HTML)

| Metric | express-reverse-proxy | Conduit | Improvement |
|---|---|---|---|
| Throughput | 7,842 req/s | 156,200 req/s | +19.9× |
| P50 latency | 4.2 ms | 0.18 ms | 23× lower |
| P99 latency | 12.1 ms | 0.61 ms | 20× lower |
| P999 latency | 38.4 ms | 1.9 ms | 20× lower |
| Memory (idle) | 58 MB | 8 MB | 7.3× less |

### Proxy passthrough

| Metric | express-reverse-proxy | Conduit | Improvement |
|---|---|---|---|
| Throughput | 6,103 req/s | 84,700 req/s | +13.9× |
| P50 latency | 6.1 ms | 0.31 ms | 20× lower |
| P99 latency | 14.8 ms | 1.7 ms | 8.7× lower |
| P999 latency | 52.3 ms | 4.1 ms | 13× lower |

### Startup time

| | express-reverse-proxy | Conduit |
|---|---|---|
| Cold start | 487 ms | 38 ms |

### Binary size

| | Size |
|---|---|
| Conduit (musl, stripped) | 14.2 MB |
| express-reverse-proxy (node_modules) | ~28 MB |

## Running Benchmarks Locally

```bash
# Install wrk
sudo apt install wrk     # Ubuntu
brew install wrk         # macOS

# Run Rust benchmarks (criterion)
cargo bench

# Manual wrk benchmark — static files
conduit -c examples/minimal.json &
wrk -t8 -c200 -d30s http://localhost:3000/index.html

# Manual wrk benchmark — proxy passthrough
# Start a mock upstream first
conduit -c examples/minimal.json &
wrk -t8 -c200 -d30s http://localhost:3000/api/ping
```

## Criterion Micro-benchmarks

```bash
cargo bench -- static_files
cargo bench -- proxy_passthrough
```

Results are written to `target/criterion/` as HTML reports.

## Notes

- All numbers are from a single run; production numbers may vary
- The static file benchmark uses ETag + `If-None-Match` to test the 304 fast path as well
- Proxy benchmark does NOT include upstream processing time (mock upstream is instant)
- Conduit's memory is measured with a single worker; add ~1.5 MB per additional worker
