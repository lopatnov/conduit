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
re-run the job or measure with `scripts/bench/ab.sh` before acting on it. A report holds eight
comparisons (four metrics at two worker settings), so one lone ⚠️ among them can still be chance:
look for the same direction in the other rows and in a second run. The load-balancing
tables below hold single 5-second runs, so they are flagged only for drops above 25%.

| | |
| --- | --- |
| Runner | GitHub-hosted `ubuntu-latest`: 4 vCPU, 16 GB RAM. GitHub assigns the host CPU generation; the report prints it (for example `4 vCPU, 16 GB RAM allocated to this job (host CPU model: …), ubuntu24 (X64)`). |
| Build | `cargo build --release` on the stable toolchain: `default` features, `lto = true`, `codegen-units = 1`, `strip = true` |
| Conduit workers | two settings: `global.workers` unset, which is **one worker thread**, and `global.workers: 4` |
| Upstream | the Go server below, port 4000, fixed 22-byte JSON body, keep-alive (it also counts its requests, for the load-balancing scenarios) |
| Load generator | [oha](https://github.com/hatoo/oha) 1.16.0 (pinned and checked against a SHA-256), 50 connections, 10 seconds |
| Reported | requests per second, p50 and p99 latency and success rate, for base and head and their difference with a verdict against the run-to-run noise, for each worker setting |

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
    "fmt"
    "net/http"
    "os"
    "strconv"
    "sync/atomic"
    "time"
)

// PORT (default 4000) and DELAY_MS (default 0) come from the environment; the load-balancing
// scenarios start four of these and read each one's request count from /__count.
func main() {
    port := os.Getenv("PORT")
    if port == "" {
        port = "4000"
    }
    delay, _ := strconv.Atoi(os.Getenv("DELAY_MS"))
    var count atomic.Int64
    body := []byte(`{"status":"ok","ts":0}`)
    http.HandleFunc("/__count", func(w http.ResponseWriter, r *http.Request) {
        fmt.Fprint(w, count.Load())
    })
    http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
        if delay > 0 {
            time.Sleep(time.Duration(delay) * time.Millisecond)
        }
        count.Add(1)
        w.Header().Set("Content-Type", "application/json")
        w.Write(body)
    })
    srv := &http.Server{Addr: ":" + port, ReadTimeout: 5 * time.Second}
    srv.ListenAndServe()
}
```

### Load-balancing scenarios

Below the passthrough tables the report has two sections **Load balancing across four upstreams**, for the
same pair of builds: one with **one worker thread** (the default) and, in addition to it, one with
**four** (`global.workers: 4`, the balancer using all the vCPUs of the runner; the four upstreams and the
load generator run on the same vCPUs). `scripts/ci/bench_lb.sh` (`BENCH_LB_WORKERS` is the worker count)
starts four copies of the upstream above (ports 4001-4004),
puts Conduit in front of them with one strategy at a time (`round-robin`, `least-conn`, `p2c`,
`least-bandwidth`), and drives it with `oha -z 5s -c 50`. Two scenarios:

- **even**: all four upstreams answer at once. It shows what a strategy costs when there is nothing to be
  clever about: the selection work, and the in-flight bookkeeping the load-aware ones do per request.
- **slow**: upstream 0 sleeps 8 ms per request. It shows what a strategy does about a slow upstream: the
  share of requests it still gets (the last column), and what that does to throughput and p99.

Each row gives requests per second and p99 for base and head with the difference, and the head build's
share of the requests per upstream, read from the upstreams' own counters. A strategy the base commit does
not have (a new one) shows `n/a` on the base side. The runs are short and the runner is shared, so a
difference of a few per cent is noise; a strategy whose share moves, or a regression flagged with ⚠️ in
both scenarios, is worth a look. The section is also written to the job summary of the run, which is where
a change to the report shows first: the PR comment is formatted by the script on the default branch.

For a longer, local comparison with failures and weights see
[Comparing the load-balancing strategies](#comparing-the-load-balancing-strategies).

## Reproduce it on your machine

Linux or macOS; needs Go, a Rust toolchain and `oha`. Save the block as `bench.sh` and run `bash bench.sh`: it stops
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
    "fmt"
    "net/http"
    "os"
    "strconv"
    "sync/atomic"
    "time"
)

// PORT (default 4000) and DELAY_MS (default 0) come from the environment; the load-balancing
// scenarios start four of these and read each one's request count from /__count.
func main() {
    port := os.Getenv("PORT")
    if port == "" {
        port = "4000"
    }
    delay, _ := strconv.Atoi(os.Getenv("DELAY_MS"))
    var count atomic.Int64
    body := []byte(`{"status":"ok","ts":0}`)
    http.HandleFunc("/__count", func(w http.ResponseWriter, r *http.Request) {
        fmt.Fprint(w, count.Load())
    })
    http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
        if delay > 0 {
            time.Sleep(time.Duration(delay) * time.Millisecond)
        }
        count.Add(1)
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
`--no-default-features --features static`, `default`, `standard` and `full`. Sizes depend on the
target (musl, glibc, Windows), so compare like with like. To measure one locally:

```bash
cargo build --release --features standard   # the workspace release profile strips symbols
ls -l target/release/conduit
```
