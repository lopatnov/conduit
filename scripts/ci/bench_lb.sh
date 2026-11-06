#!/usr/bin/env bash
# Load-balancing scenarios for the CI performance report (issue #629, PR #631).
#
#   scripts/ci/bench_lb.sh LABEL OUT_JSON
#
# Run from the repository root, after `cargo build --release`, once per build being compared (head and base).
# For each scenario and each strategy it starts four upstreams, starts Conduit with that strategy over them,
# drives it with `oha` for BENCH_LB_SECS seconds (default 5) with 50 connections, and records requests per
# second, p50 and p99 latency, and each upstream's share of the requests (read from the upstreams' own
# counters). A strategy the build does not know (the base commit has no `least-bandwidth`) fails to start; it
# is recorded as null and the others continue.
#
#   even  four equal upstreams: what a strategy costs when there is nothing to be clever about
#   slow  upstream 0 takes BENCH_LB_SLOW_MS (default 8) ms per request, the others answer at once: what a
#         strategy does about a slow upstream (its share of the traffic, and what that does to throughput
#         and the tail)
#
# Writes {"even": {"<strategy>": {"rps", "p50", "p99", "ok", "share": [..]} | null, ..}, "slow": {..}} to
# OUT_JSON. Binaries can be overridden for testing: CONDUIT_BIN, UPSTREAM_BIN, OHA; also BENCH_LB_STRATEGIES,
# BENCH_LB_SECS, BENCH_LB_CONNS, BENCH_LB_SLOW_MS, BENCH_LB_WORKERS.
#
# BENCH_LB_WORKERS is Conduit's `global.workers` (default 1: the setting left unset, one worker thread). The CI
# job runs the script once with 1 and once with 4, the vCPUs of its runner (shared with the four upstreams and the
# load generator, so it shows how a strategy behaves when the balancer's workers compete for the machine).
#
# The numbers come from a shared CI runner: read them as a trend (base against head, back to back on the
# same machine), not as a lab measurement.
set -uo pipefail

LABEL="${1:?usage: bench_lb.sh LABEL OUT_JSON}"
OUT="${2:?usage: bench_lb.sh LABEL OUT_JSON}"
SECS="${BENCH_LB_SECS:-5}"
SLOW_MS="${BENCH_LB_SLOW_MS:-8}"
CONNS="${BENCH_LB_CONNS:-50}"
WORKERS="${BENCH_LB_WORKERS:-1}"
case "$WORKERS" in ''|*[!0-9]*|0*) echo "BENCH_LB_WORKERS must be a positive integer, got '$WORKERS'" >&2; exit 2 ;; esac
CONDUIT_BIN="${CONDUIT_BIN:-./target/release/conduit}"
UPSTREAM_BIN="${UPSTREAM_BIN:-/tmp/bench-upstream-bin}"
OHA="${OHA:-oha}"
PROXY_PORT="${BENCH_LB_PROXY_PORT:-8080}"
UP_PORTS=(${BENCH_LB_UPSTREAM_PORTS:-4001 4002 4003 4004})
STRATEGIES=(${BENCH_LB_STRATEGIES:-round-robin least-conn p2c least-bandwidth})

WORK="$(mktemp -d)"
RESULTS="$WORK/results.tsv"
: > "$RESULTS"
UP_PIDS=()  # the upstreams that are running now; emptied when they are stopped
CPID=""     # the Conduit that is running now; emptied when it is reaped
cleanup() {
  [ -n "$CPID" ] && kill -KILL "$CPID" 2>/dev/null
  for p in "${UP_PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  # Only our own children: a bare `wait` would also block on anything else the shell started.
  [ -n "$CPID" ] && wait "$CPID" 2>/dev/null
  [ "${#UP_PIDS[@]}" -gt 0 ] && wait "${UP_PIDS[@]}" 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT

wait_for() { # URL, tries
  local url="$1" tries="${2:-30}"
  for _ in $(seq 1 "$tries"); do
    curl -sf "$url" > /dev/null 2>&1 && return 0
    sleep 0.5
  done
  return 1
}

start_upstreams() { # DELAY_MS of upstream 0
  UP_PIDS=()
  local i=0 delay
  for port in "${UP_PORTS[@]}"; do
    delay=0
    [ "$i" -eq 0 ] && delay="$1"
    PORT="$port" DELAY_MS="$delay" "$UPSTREAM_BIN" &
    UP_PIDS+=("$!")
    i=$((i + 1))
  done
  for port in "${UP_PORTS[@]}"; do
    wait_for "http://127.0.0.1:$port/" 30 || { echo "upstream on $port never became ready" >&2; return 1; }
  done
}

stop_upstreams() {
  for p in "${UP_PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait "${UP_PIDS[@]:-}" 2>/dev/null || true
  UP_PIDS=()
}

counts() { # one request count per line, in port order
  for port in "${UP_PORTS[@]}"; do curl -sf "http://127.0.0.1:$port/__count" || echo 0; echo; done | grep -v '^$' | tr '\n' ' '
}

run_one() { # SCENARIO STRATEGY
  local scenario="$1" strategy="$2" targets="" port
  for port in "${UP_PORTS[@]}"; do targets="$targets\"http://127.0.0.1:$port\","; done
  # One worker thread is the default, so `global` is written only for more: the one-worker run is byte for byte
  # the configuration it always was.
  local global=""
  [ "$WORKERS" -ne 1 ] && global="\"global\":{\"workers\":$WORKERS},"
  printf '{%s"sites":[{"port":%s,"proxy":{"/":{"targets":[%s],"strategy":"%s"}}}]}' \
    "$global" "$PROXY_PORT" "${targets%,}" "$strategy" > "$WORK/conduit.json"

  "$CONDUIT_BIN" -c "$WORK/conduit.json" > "$WORK/conduit.log" 2>&1 &
  local cpid=$! ready=0
  CPID="$cpid"
  for _ in $(seq 1 20); do
    kill -0 "$cpid" 2>/dev/null || break # exited: the build does not know this strategy
    if curl -sf "http://127.0.0.1:$PROXY_PORT/__health__" > /dev/null 2>&1; then ready=1; break; fi
    sleep 0.5
  done
  # Conduit is stopped with SIGKILL, here and below: on SIGTERM Pingora sleeps the whole shutdown grace period
  # (30 s by default), which over sixteen runs would add minutes to the job. Nothing here needs a graceful stop.
  if [ "$ready" -ne 1 ]; then
    kill -KILL "$cpid" 2>/dev/null || true
    wait "$cpid" 2>/dev/null || true
    CPID=""
    printf '%s\t%s\tnull\n' "$scenario" "$strategy" >> "$RESULTS"
    echo "  $LABEL $scenario/$strategy: not started (unknown strategy for this build?)" >&2
    return 0
  fi

  local before after
  before="$(counts)"
  "$OHA" -z "${SECS}s" -c "$CONNS" --no-tui --output-format json "http://127.0.0.1:$PROXY_PORT/" > "$WORK/oha.json" 2> "$WORK/oha.err" || true
  after="$(counts)"
  kill -KILL "$cpid" 2>/dev/null || true
  wait "$cpid" 2>/dev/null || true
  CPID=""

  python3 - "$scenario" "$strategy" "$WORK/oha.json" "$before" "$after" >> "$RESULTS" <<'PY'
import json, sys
scenario, strategy, path, before, after = sys.argv[1:6]
try:
    m = json.load(open(path))["metrics"]
    b = [int(x) for x in before.split()]
    a = [int(x) for x in after.split()]
    d = [max(0, y - x) for x, y in zip(b, a)]
    total = sum(d) or 1
    row = {
        "rps": round(m["requests_per_sec"]),
        "p50": round(m["latency_ms"]["p50"], 2),
        "p99": round(m["latency_ms"]["p99"], 2),
        "ok": round(m["success_rate"] * 100, 1),
        "share": [round(x / total * 100, 1) for x in d],
    }
    print(f"{scenario}\t{strategy}\t{json.dumps(row, separators=(',', ':'))}")
except Exception as e:  # a failed measurement is a null, not a failed job
    print(f"{scenario}\t{strategy}\tnull")
    print(f"  {scenario}/{strategy}: no usable measurement ({e})", file=sys.stderr)
PY
}

for scenario in even slow; do
  delay=0
  [ "$scenario" = slow ] && delay="$SLOW_MS"
  # A start failure can leave some upstreams running: stop them, or the next scenario would find them on
  # the ports and measure the wrong delay.
  start_upstreams "$delay" || { echo "skipping scenario $scenario" >&2; stop_upstreams; continue; }
  for strategy in "${STRATEGIES[@]}"; do
    run_one "$scenario" "$strategy"
  done
  stop_upstreams
done

python3 - "$RESULTS" "$OUT" <<'PY'
import json, sys
out = {"even": {}, "slow": {}}
for line in open(sys.argv[1], encoding="utf-8"):
    scenario, strategy, payload = line.rstrip("\n").split("\t", 2)
    out.setdefault(scenario, {})[strategy] = json.loads(payload)
json.dump(out, open(sys.argv[2], "w", encoding="utf-8"), separators=(",", ":"))
PY
status=$?
if [ "$status" -ne 0 ]; then
  echo "load-balancing results ($LABEL) could not be written" >&2
  exit "$status"
fi
echo "load-balancing results ($LABEL) written to $OUT"
