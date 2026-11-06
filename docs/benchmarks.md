# Benchmarks

How Conduit's performance is measured and how to repeat the measurement on your own machine.

This page contains no throughput or latency figures. The ones that used to be here were measured
once, on one machine, and could not be reproduced later (issues #475 and #487), so they were
removed rather than left to mislead. Every number you quote should come from a command on this
page, together with the hardware, the build and `global.workers` it was measured with.

## Table of Contents

- [CI performance report (exact setup)](#ci-performance-report-exact-setup)
- [Why the CI report measures one worker thread and four](#why-the-ci-report-measures-one-worker-thread-and-four)
- [Reproduce it on your machine](#reproduce-it-on-your-machine)
- [Measuring more than passthrough](#measuring-more-than-passthrough)
- [Comparing the load-balancing strategies](#comparing-the-load-balancing-strategies)
- [Comparing Conduit with another proxy](#comparing-conduit-with-another-proxy)
- [Binary size and dependencies](#binary-size-and-dependencies)

---

## CI performance report (exact setup)

Every pull request runs the **Performance report** job (`.github/workflows/ci.yml`). It builds the
PR head and the PR's base commit **on the same runner**, measures reverse-proxy passthrough for
each, and posts the difference as a PR comment. It is informational: nothing is gated on it, and a
shared runner is too noisy for absolute numbers, so read the difference between base and head
rather than comparing figures across pull requests. If the workflow and this section ever
disagree, the workflow is right.

Each number is the **mean of four 10-second runs** per build, taken alternately (head, base, base,
head, head, base, base, head), so that a slow start or a drifting runner hits both builds alike.
This is done once with **one worker thread** (the default) and once with **four**
(`global.workers: 4`).

One 10-second run on a shared runner scatters by several percent: on a pinned 8-vCPU WSL2 machine,
24 runs of two copies of the *same* binary had a standard deviation of 6–9% of the requests per
second (from 13.7k to 17.5k), and the two copies differed by 0.0% on average. So the
Δ column says whether a difference is larger than the noise: the job computes the standard error
of each mean from the scatter of its runs, and the report calls a difference *within noise* when
it is smaller than 2.5 times the standard error of the difference. Only an adverse difference beyond
the noise (and at least 3%) is flagged ⚠️. A drop of 6% that is within noise is not a finding;
re-run the job or measure with `scripts/bench/ab.sh` before acting on it. A report holds four
comparisons (throughput and p99 latency at two worker settings), so one lone ⚠️ among them can still be chance:
look for the same direction in the other rows and in a second run.

| | |
| --- | --- |
| Runner | GitHub-hosted `ubuntu-latest`: 4 vCPU, 16 GB RAM. GitHub assigns the host CPU generation; the report prints it (for example `4 vCPU, 16 GB RAM allocated to this job (host CPU model: …), ubuntu24 (X64)`). |
| Build | `cargo build --release` on the stable toolchain: `default` features, `lto = true`, `codegen-units = 1`, `strip = true` |
| Conduit workers | two settings: `global.workers` unset, which is **one worker thread**, and `global.workers: 4` |
| Upstream | the Go server below, port 4000, fixed 22-byte JSON body, keep-alive |
| Load generator | [oha](https://github.com/hatoo/oha) 1.16.0 (pinned and checked against a SHA-256), 50 connections, 10 seconds |
| Reported | requests per second and p99 latency, for base and head and their difference with a verdict against the run-to-run noise, for each worker setting; the success rate only when a request failed (the median latency is measured but not shown: at a fixed concurrency it only restates the throughput) |

**Server configuration** — CI writes it as JSON; the YAML is the same configuration:

```json
{ "port": 8080, "proxy": "http://127.0.0.1:4000" }
```

```yaml
port: 8080
proxy: http://127.0.0.1:4000
```

The four-worker run uses the same site with `global.workers` set:

```json
{ "global": { "workers": 4 }, "sites": [ { "port": 8080, "proxy": "http://127.0.0.1:4000" } ] }
```

**Upstream** (`main.go`):

```go
package main

import (
    "net/http"
    "os"
    "time"
)

// PORT (default 4000) comes from the environment.
func main() {
    port := os.Getenv("PORT")
    if port == "" {
        port = "4000"
    }
    body := []byte(`{"status":"ok","ts":0}`)
    http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
        w.Header().Set("Content-Type", "application/json")
        w.Write(body)
    })
    srv := &http.Server{Addr: ":" + port, ReadTimeout: 5 * time.Second}
    srv.ListenAndServe()
}
```

### Why the report has no load-balancing tables

An earlier version of the report also ran each balancing strategy through four upstreams. It was removed:
the four upstreams, the load generator and Conduit's workers all share the runner's four vCPUs, so the
numbers cannot separate what a strategy costs from how the machine was divided (the same build reached 10 to
20% fewer requests per second through four upstreams than through one, for every strategy including
round-robin; the cause is open, see #647). To compare strategies use
[`scripts/lb_compare.py`](#comparing-the-load-balancing-strategies) on a machine of your own, ideally with the
upstreams on other cores than Conduit.

## Reproduce it on your machine

Linux or macOS; needs Go, a Rust toolchain, Python 3 and `oha`. Save the block as `bench.sh` and run `bash bench.sh`: it stops
at the first failure, and on the way out it stops the two background processes it started and removes its scratch
directory.

```bash
set -eu
WORK=$(mktemp -d)   # a private scratch directory, not a predictable name in /tmp
# `jobs -p` lists only children that are still running, so no stale process number is ever signalled
trap 'kill $(jobs -p) 2>/dev/null; rm -r -- "${WORK:?}"' EXIT

# 1. the upstream: the same program as above, written to the scratch directory
cat > "$WORK/main.go" <<'GOEOF'
package main

import (
    "net/http"
    "os"
    "time"
)

// PORT (default 4000) comes from the environment.
func main() {
    port := os.Getenv("PORT")
    if port == "" {
        port = "4000"
    }
    body := []byte(`{"status":"ok","ts":0}`)
    http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
        w.Header().Set("Content-Type", "application/json")
        w.Write(body)
    })
    // CI listens on all interfaces (":" + port); a run on a laptop binds to loopback only
    srv := &http.Server{Addr: "127.0.0.1:" + port, ReadTimeout: 5 * time.Second}
    srv.ListenAndServe()
}
GOEOF
(cd "$WORK" && go mod init bench-upstream && go build -o bench-upstream .)
"$WORK"/bench-upstream &
UP=$!
sleep 1; kill -0 "$UP"   # stops the script here if the upstream did not start (is port 4000 in use?)

# 2. Conduit, built and configured exactly as in CI
cd /path/to/conduit && cargo build --release
printf '{ "port": 8080, "proxy": "http://127.0.0.1:4000" }' > "$WORK/conduit.json"
./target/release/conduit -c "$WORK/conduit.json" &
CP=$!
curl -sf http://127.0.0.1:8080/__health__   # wait until this answers

# 3. the load
oha -z 10s -c 50 --no-tui --output-format json http://127.0.0.1:8080/ \
  | python3 -c 'import json,sys; m=json.load(sys.stdin)["metrics"]; print(round(m["requests_per_sec"]), "req/s, p50", round(m["latency_ms"]["p50"],2), "ms, p99", round(m["latency_ms"]["p99"],2), "ms, success", m["success_rate"])'
```

Record `nproc`, the CPU model (`grep -m1 'model name' /proc/cpuinfo`), the RAM, the Conduit
commit and `global.workers` next to any number you publish.

## Why the CI report measures one worker thread and four

Conduit starts one worker thread unless `global.workers` says otherwise (Pingora's default). The
first table of the Performance report measures that configuration, which is what most people run,
so that a pull request which makes the default path slower shows up as a lower number next to its
base. The second table sets `global.workers: 4`, the number of vCPUs of the runner, to catch a
change that costs nothing on one thread but hurts when the workers share state or compete for the
machine. The load generator and the upstream run on the same four vCPUs, so the second table says
how a build behaves when its workers compete for the machine, not what four free cores would give;
it cannot show how Conduit scales on a big host. For that, set `global.workers` to the number of
cores you can spare and measure on your own machine; `scripts/bench/matrix.sh` runs several worker
counts in one go.

## Measuring more than passthrough

[`scripts/bench/`](../scripts/bench/README.md) goes beyond the CI scenario. It reports **CPU time
per request** next to requests per second, which is comparable across machines and worker counts
where requests per second is not, runs several `global.workers` values and access-log settings,
compares two binaries with alternating runs (`ab.sh`), and builds the "floors": a raw TCP relay
and the smallest possible Pingora proxy, which show how much of a request's cost is the
operating system and Pingora rather than Conduit.

To find what one feature costs, run the passthrough scenario twice, once without it and once with
it configured, on the same build and the same worker count.

## Comparing the load-balancing strategies

`scripts/lb_compare.py` runs every strategy through four scenarios (mixed light and heavy requests, one
slow upstream, weights 3:1:1, an upstream that dies and comes back) and prints Markdown tables of
throughput, latency percentiles, each upstream's share and how much that share swings from second to
second. It starts its own mock upstreams and the Conduit binary, and needs only Python 3.11+:

```bash
python scripts/lb_compare.py --duration 10                   # all of it, about 8 minutes
python scripts/lb_compare.py --strategies least-conn,p2c --scenarios slow-one
python scripts/lb_compare.py --health-checks --strategies round-robin,least-response-time
```

`--health-checks` gives every route an active health check (one `HEAD` probe a second). Without it
`least-response-time` has no latencies to go on and behaves like round-robin, and a dead upstream is found
only by passive ejection; with it the harness measures what a deployment with health checks sees.

It uses a debug build when there is no release one, and each cell is a single run: throughput varies by
10–20% from run to run on a desktop (round-robin on the mixed scenario gave 1504 to 1867 requests per second over five runs), so only compare differences well beyond that, and repeat a
run before drawing a conclusion from a small one. The docstring of the script describes the scenarios.

## Comparing Conduit with another proxy

A comparison is only fair when everything but the proxy is the same:

- the same machine, with the proxy, the upstream and the load generator pinned to separate CPUs;
- the same number of worker threads or processes: set `global.workers` for Conduit and
  `worker_processes` (nginx) or the equivalent for the other proxy, and report the number;
- the same upstream, response body, keep-alive behaviour, connection count and duration;
- release builds, with the versions and the full configuration of both proxies published;
- CPU time per request next to requests per second.

## Binary size and dependencies

Pull requests get a **Footprint report** comment (`.github/workflows/ci.yml`; on a pull request from a fork the
comment cannot be posted and the report stays in the job log): the stripped
release binary size and the number of normal dependencies (`cargo tree -e normal | wc -l`, a rough measure) for `--no-default-features`,
`--no-default-features --features static`, `default`, the two bundles for `--no-default-features` builds
(`static-server`, `gateway`), `standard` and `full`. Sizes depend on the
target (musl, glibc, Windows), so compare like with like. To measure one locally:

```bash
cargo build --release --features standard   # the workspace release profile strips symbols
ls -l target/release/conduit
```
