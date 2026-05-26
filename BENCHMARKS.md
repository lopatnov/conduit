# Benchmarks

> **These are design targets, not yet measured results.**
>
> Conduit is currently in active development (v0.3.x). The numbers below are the
> performance goals the project is designed to reach. Real benchmark results will be
> added here as development stabilises — contributions welcome.

## Performance Targets

Test environment (planned): Linux x86-64, 4 cores, `wrk -t8 -c200 -d30s`.

| Scenario                      | Target                |
| ----------------------------- | --------------------- |
| Static file 1 KB              | **≥ 150,000 req/s**   |
| Proxy passthrough             | **≥ 80,000 req/s**    |
| P99 proxy latency             | **≤ 2 ms**            |
| Memory (idle, 1 site)         | **≤ 10 MB**           |
| Cold start time               | **≤ 50 ms**           |
| Binary size (musl, stripped)  | **≤ 15 MB**           |

## Why these numbers are achievable

- [Cloudflare Pingora](https://github.com/cloudflare/pingora) routes ~1 trillion requests per day in production on the same architecture
- Static files are served directly inside Pingora's hot path — no IPC, no middleware stack overhead
- Connection pool reuse means near-zero heap allocations per request in steady state
- `lto = true` + `codegen-units = 1` + `strip = true` in the release profile
- Rust has no GC pauses, no JIT warm-up period, no event-loop overhead

## Running Benchmarks Yourself

### Micro-benchmarks (no external tool required)

```bash
cargo bench
```

### HTTP load test — static files

```bash
cargo build --release
./target/release/conduit -c examples/minimal.json &
wrk -t8 -c200 -d30s http://localhost:8080/
```

### HTTP load test — proxy passthrough

```bash
# Start a minimal upstream (e.g. python -m http.server 4000)
./target/release/conduit -c examples/spa-with-api.json &
wrk -t8 -c200 -d30s http://localhost:8080/api/ping
```

Install wrk: `sudo apt install wrk` (Ubuntu) · `brew install wrk` (macOS)

## Submitting Results

If you run Conduit in a reproducible environment and want to share results,
open a pull request editing this file. Please include:

- OS, CPU model, RAM
- `wrk` version and exact command
- Conduit version (`conduit --version`)
- Config file used
