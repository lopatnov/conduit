# Benchmarks

> **These are design targets, not measured results.**
>
> Conduit is currently in early development (Phases 1–2). The numbers below are the
> performance goals the project is designed to hit once fully implemented. They will be
> replaced with real wrk/criterion output as development progresses.
>
> Contributions of real benchmark runs are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).

## Design Targets

Measured environment (planned): Linux x86-64, `wrk -t8 -c200 -d30s`,
compared to `express-reverse-proxy` running an identical workload.

| Scenario | express-reverse-proxy | Conduit target |
|---|---|---|
| Static file 1 KB | ~8,000 req/s | **≥ 150,000 req/s** |
| Proxy passthrough | ~6,000 req/s | **≥ 80,000 req/s** |
| P99 proxy latency | ~15 ms | **≤ 2 ms** |
| Memory (idle) | ~60 MB | **≤ 10 MB** |
| Startup time | ~500 ms | **≤ 50 ms** |
| Binary size (musl, stripped) | N/A | **≤ 15 MB** |

## Rationale

These targets are achievable because:

- Pingora (Cloudflare) processes millions of req/s in production on the same architecture
- Static files are served directly in Pingora's hot path — no IPC, no extra process
- Zero heap allocations per request in the steady state (connection pool reuse)
- `lto = true` + `codegen-units = 1` + `strip = true` in release profile
- Rust has no GC pauses, no JIT warm-up, no event loop overhead

## Running Benchmarks

```bash
# Micro-benchmarks (criterion, no external tool needed)
cargo bench

# Manual wrk benchmark — static files
cargo build --release
./target/release/conduit -c examples/minimal.json &
wrk -t8 -c200 -d30s http://localhost:8080/

# Manual wrk benchmark — proxy passthrough
./target/release/conduit -c examples/minimal.json &
wrk -t8 -c200 -d30s http://localhost:8080/api/ping
```

Install wrk: `sudo apt install wrk` (Ubuntu) · `brew install wrk` (macOS)
