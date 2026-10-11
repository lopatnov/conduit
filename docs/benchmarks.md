# Benchmarks

How Conduit's performance is measured and how to repeat the measurement on your own machine.

This page contains no throughput or latency figures. The ones that used to be here were measured
once, on one machine, and could not be reproduced later (issues #475 and #487), so they were
removed rather than left to mislead. Every number you quote should come from a command on this
page, together with the hardware, the build and `global.workers` it was measured with.

## Table of Contents

- [CI performance report (exact setup)](#ci-performance-report-exact-setup)
- [Why the CI report uses one worker thread](#why-the-ci-report-uses-one-worker-thread)
- [Reproduce it on your machine](#reproduce-it-on-your-machine)
- [Measuring more than passthrough](#measuring-more-than-passthrough)
- [Comparing Conduit with another proxy](#comparing-conduit-with-another-proxy)
- [Binary size and dependencies](#binary-size-and-dependencies)

---

## CI performance report (exact setup)

Every pull request runs the **Performance report** job (`.github/workflows/ci.yml`). It builds the
PR head and the PR's base commit one after the other **on the same runner**, measures
reverse-proxy passthrough for each, and posts the difference as a PR comment. It is informational:
nothing is gated on it, and a shared runner is too noisy for absolute numbers, so read the
difference between base and head rather than comparing figures across pull requests. If the
workflow and this section ever disagree, the workflow is right.

| | |
| --- | --- |
| Runner | GitHub-hosted `ubuntu-latest`: 4 vCPU, 16 GB RAM. GitHub assigns the host CPU generation; the report prints it (for example `4 vCPU, 16 GB RAM allocated to this job (host CPU model: …), ubuntu24 (X64)`). |
| Build | `cargo build --release` on the stable toolchain: `default` features, `lto = true`, `codegen-units = 1`, `strip = true` |
| Conduit workers | `global.workers` unset, which is **one worker thread** |
| Upstream | the Go server below, port 4000, fixed 22-byte JSON body, keep-alive |
| Load generator | [oha](https://github.com/hatoo/oha) 1.16.0 (pinned and checked against a SHA-256), 50 connections, 10 seconds |
| Reported | requests per second, p50 and p99 latency, success rate, for base and head and their difference |

**Server configuration** — CI writes it as JSON; the YAML is the same configuration:

```json
{ "port": 8080, "proxy": "http://127.0.0.1:4000" }
```

```yaml
port: 8080
proxy: http://127.0.0.1:4000
```

**Upstream** (`main.go`):

```go
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

## Reproduce it on your machine

Linux or macOS; needs Go, a Rust toolchain and `oha`:

```bash
WORK=$(mktemp -d)   # a private scratch directory, not a predictable name in /tmp

# 1. the upstream: the same program as above, written to the scratch directory
cat > "$WORK/main.go" <<'GOEOF'
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
GOEOF
(cd "$WORK" && go mod init bench-upstream && go build -o bench-upstream .)
"$WORK"/bench-upstream &

# 2. Conduit, built and configured exactly as in CI
cd /path/to/conduit && cargo build --release
printf '{ "port": 8080, "proxy": "http://127.0.0.1:4000" }' > "$WORK/conduit.json"
./target/release/conduit -c "$WORK/conduit.json" &
curl -sf http://127.0.0.1:8080/__health__   # wait until this answers

# 3. the load
oha -z 10s -c 50 --no-tui --output-format json http://127.0.0.1:8080/ \
  | python3 -c 'import json,sys; m=json.load(sys.stdin)["metrics"]; print(round(m["requests_per_sec"]), "req/s, p50", round(m["latency_ms"]["p50"],2), "ms, p99", round(m["latency_ms"]["p99"],2), "ms, success", m["success_rate"])'
```

Record `nproc`, the CPU model (`grep -m1 'model name' /proc/cpuinfo`), the RAM, the Conduit
commit and `global.workers` next to any number you publish. Stop the background processes and
remove the scratch directory afterwards: `kill %1 %2; rm -r "$WORK"`.

## Why the CI report uses one worker thread

Conduit starts one worker thread unless `global.workers` says otherwise (Pingora's default). The
Performance report measures the configuration most people run, so that a pull request which makes
the default path slower shows up as a lower number next to its base. It says nothing about how
Conduit scales with cores. To see scaling, set `global.workers` (for example to the number of
cores you can spare) and measure again; `scripts/bench/matrix.sh` runs several worker counts in
one go.

## Measuring more than passthrough

[`scripts/bench/`](../scripts/bench/README.md) goes beyond the CI scenario. It reports **CPU time
per request** next to requests per second, which is comparable across machines and worker counts
where requests per second is not, runs several `global.workers` values and access-log settings,
compares two binaries with alternating runs (`ab.sh`), and builds the "floors": a raw TCP relay
and the smallest possible Pingora proxy, which show how much of a request's cost is the
operating system and Pingora rather than Conduit.

To find what one feature costs, run the passthrough scenario twice, once without it and once with
it configured, on the same build and the same worker count.

## Comparing Conduit with another proxy

A comparison is only fair when everything but the proxy is the same:

- the same machine, with the proxy, the upstream and the load generator pinned to separate CPUs;
- the same number of worker threads or processes: set `global.workers` for Conduit and
  `worker_processes` (nginx) or the equivalent for the other proxy, and report the number;
- the same upstream, response body, keep-alive behaviour, connection count and duration;
- release builds, with the versions and the full configuration of both proxies published;
- CPU time per request next to requests per second.

## Binary size and dependencies

Every pull request gets a **Footprint report** comment (`.github/workflows/ci.yml`): the stripped
release binary size and the number of normal dependencies for `--no-default-features`,
`--no-default-features --features static`, `default`, `standard` and `full`. Sizes depend on the
target (musl, glibc, Windows), so compare like with like. To measure one locally:

```bash
cargo build --release --features standard   # the workspace release profile strips symbols
ls -l target/release/conduit
```
