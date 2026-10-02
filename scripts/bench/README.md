# Throughput and CPU-cost benchmarks (issue #475)

Scripts for measuring reverse-proxy **passthrough** — one HTTP request through Conduit to a fixed-body keep-alive upstream —
and for finding out where the CPU time of a request goes. They were written to investigate why the CI "Performance report"
showed ~8.7k req/s and are what the numbers in [`docs/benchmarks.md`](../../docs/benchmarks.md)'s reproducibility note come from.
Linux only (they read `/proc`, pin processes with `taskset`); on Windows run them inside WSL2 on the WSL file system.

| Script | What it answers |
|---|---|
| `matrix.sh BIN OUT [ROUNDS]` | Requests/s, p50/p99 and **CPU µs per request** of one Conduit binary for several `global.workers` values, access log on/off/piped, 50 or 200 connections. `ONLY="w1-nolog w4-nolog"` runs a subset. |
| `ab.sh BIN_A BIN_B [ROUNDS]` | The same matrix for two binaries, alternating A B B A within each round. Use it for "does this change make requests cheaper?". |
| `probe.sh BIN [ROUNDS]` | CPU per request split into user and kernel time and per thread, for Conduit and the two **floors** below (`TARGETS="conduit pp relay"`). Shows whether work runs where `global.workers` says. |
| `size.sh`, `bloatdiff.sh` | What is in the `--no-default-features` binary (#516): stripped size, `cargo bloat` per crate, and the same build of the smallest Pingora proxy as the floor; `bloatdiff.sh` prints the per-crate difference. Needs `cargo-bloat` (`size.sh` installs it) and a `~/perf/pp` crate built from `floors/pingora_min.rs`. |
| `floors/mock.rs` | The upstream: a multi-thread tokio HTTP server answering every request with the same 22-byte JSON body. |
| `floors/relay.rs` | A raw single-thread TCP relay `:8080 → :4000`: the syscall + loopback floor of any proxy. |
| `floors/pingora_min.rs` | The smallest Pingora `ProxyHttp` proxy (defaults, one worker): what Pingora itself costs, without Conduit. |

## Layout the scripts expect (`$PERF`, default `$HOME/perf`; `export PERF=…` to put it elsewhere)

```
$PERF/tools/bin/oha                       # cargo install oha --version 1.16.0 --locked --root $PERF/tools
$PERF/mock/target/release/bench-upstream  # floors/mock.rs   (tokio: rt-multi-thread, net, io-util, macros)
$PERF/relay/target/release/relay          # floors/relay.rs  (tokio: rt, net, io-util, macros)
$PERF/pp/target/release/pp                # floors/pingora_min.rs (pingora-core + pingora-proxy 0.9, feature rustls; async-trait)
$PERF/bin/conduit-*                       # release builds to compare (any path can be passed as BIN)
```

Processes are pinned to disjoint CPUs (`CPU_CONDUIT=4-11 CPU_UP=12-15 CPU_LOAD=16-21` by default — adjust to the machine,
and keep the three sets apart). Each run warms up for 4 s and measures for 10 s; the CPU cost is the change of the proxy
process's `utime + stime` (all threads) over the measured window divided by the requests served. Kill with `SIGKILL`
between runs (a `SIGTERM` starts Pingora's graceful shutdown and waits out its grace period).

## What was found with them (issue #475, WSL2, Ryzen 9 5950X, `oha -c 50`)

| | req/s | CPU per request | threads alive under load |
|---|---:|---:|---:|
| raw TCP relay, 1 thread | ~42,000 | 24 µs (2 user + 22 kernel) | 1 |
| smallest Pingora proxy, 1 worker | ~14,800 | 67 µs (31 + 36) | 4 |
| Conduit before the fix, `workers: 1` | ~9,800 | 110 µs (52 + 59) | 11–12 |
| Conduit after the fix, `workers: 1` | ~12,500 | 80 µs (41 + 38) | 5 |

`workers: 1 → 8`: 11k → 31k req/s. The access log makes no measurable difference. The cause of the extra threads and the ~30 µs
was `tokio::task::block_in_place` around the response filter chain on every response.
