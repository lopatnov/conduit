#!/usr/bin/env bash
# Linux or WSL2. Throughput matrix for issue #475: how do workers, the access log and the number of connections change
# reverse-proxy passthrough throughput, and how much CPU does one request cost?
#   matrix.sh BIN OUT [ROUNDS]
# Same scenario as CI's "Performance report" (`oha -z 10s`, passthrough to a fixed 22-byte JSON upstream), with a 4 s warm-up,
# processes pinned to disjoint cores, variants rotated per round so drift and first-run penalties spread evenly.
set -uo pipefail
WORK=$(mktemp -d)   # scratch files (config, oha output) live here, not at fixed /tmp paths
trap 'kill $(jobs -p) 2>/dev/null; rm -rf "$WORK"' EXIT   # an interrupted run leaves no upstream/proxy on the ports
PERF=${PERF:-$HOME/perf}
OHA=$PERF/tools/bin/oha
MOCK=$PERF/mock/target/release/bench-upstream
BIN=${1:?usage: matrix.sh BIN [OUT] [ROUNDS]}
OUT=${2:-$PERF/matrix.csv}
ROUNDS=${3:-3}
CPU_CONDUIT=${CPU_CONDUIT:-4-11}
CPU_UP=${CPU_UP:-12-15}
CPU_LOAD=${CPU_LOAD:-16-21}

# name|workers|logging|stdout sink|connections
VARIANTS=(
  "w1-log-devnull|1|on|devnull|50"
  "w1-log-pipe|1|on|pipe|50"
  "w1-nolog|1|off|devnull|50"
  "w2-nolog|2|off|devnull|50"
  "w4-nolog|4|off|devnull|50"
  "w8-nolog|8|off|devnull|50"
  "w4-log-pipe|4|on|pipe|50"
  "w8-log-pipe|8|on|pipe|50"
  "w8-nolog-c200|8|off|devnull|200"
)

if [ -n "${ONLY:-}" ]; then   # ONLY="w1-nolog w4-nolog": run just these variants
  keep=(); for v in "${VARIANTS[@]}"; do for o in $ONLY; do [ "${v%%|*}" = "$o" ] && keep+=("$v"); done; done
  VARIANTS=("${keep[@]}")
fi
pkill -x bench-upstream 2>/dev/null; sleep 0.2
echo "round,variant,workers,connections,rps,p50_ms,p99_ms,ok_pct,cpu_us_per_req" > "$OUT"

wait_http() {  # url
  for _ in $(seq 1 60); do curl -sf "$1" >/dev/null 2>&1 && return 0; sleep 0.25; done
  echo "not ready: $1" >&2; return 1
}
ticks() { awk '{print $14 + $15}' "/proc/$1/stat"; }   # utime + stime of the process, in clock ticks (100/s)

run_one() {  # round spec
  local round=$1 spec=$2 name workers logging sink conns
  IFS='|' read -r name workers logging sink conns <<< "$spec"
  local site='"port": 8080, "proxy": "http://127.0.0.1:4000"'
  [ "$logging" = off ] && site="$site, \"logging\": false"
  printf '{ "global": { "workers": %s }, "sites": [ { %s } ] }' "$workers" "$site" > "$WORK/conduit.json"

  taskset -c "$CPU_UP" "$MOCK" & local up=$!
  wait_http http://127.0.0.1:4000/ || { kill -9 $up 2>/dev/null; return 1; }
  local cd
  if [ "$sink" = pipe ]; then
    taskset -c "$CPU_CONDUIT" "$BIN" -c "$WORK/conduit.json" > >(cat > /dev/null) 2>&1 & cd=$!
  else
    taskset -c "$CPU_CONDUIT" "$BIN" -c "$WORK/conduit.json" > /dev/null 2>&1 & cd=$!
  fi
  wait_http http://127.0.0.1:8080/__health__ || { echo "$name: conduit did not start" >&2; kill -9 $cd $up 2>/dev/null; return 1; }

  taskset -c "$CPU_LOAD" "$OHA" -z 4s -c "$conns" --no-tui http://127.0.0.1:8080/ > /dev/null 2>&1
  local t0; t0=$(ticks $cd)
  taskset -c "$CPU_LOAD" "$OHA" -z 10s -c "$conns" --no-tui --output-format json http://127.0.0.1:8080/ > "$WORK/oha.json" 2>/dev/null
  local t1; t1=$(ticks $cd)
  kill -9 $cd $up 2>/dev/null; wait $cd $up 2>/dev/null

  python3 - "$round" "$name" "$workers" "$conns" "$t0" "$t1" "$WORK/oha.json" >> "$OUT" <<'PY'
import json, sys
round_, name, workers, conns, t0, t1, oha = sys.argv[1:8]
m = json.load(open(oha))['metrics']
rps = m["requests_per_sec"]
cpu_s = (int(t1) - int(t0)) / 100.0
us_per_req = cpu_s / (rps * 10.0) * 1e6 if rps else 0
print(",".join([round_, name, workers, conns, str(round(rps)), str(round(m["latency_ms"]["p50"], 3)),
                str(round(m["latency_ms"]["p99"], 3)), str(round(m["success_rate"] * 100, 2)), str(round(us_per_req, 1))]))
PY
  sleep 2
}

n=${#VARIANTS[@]}
for r in $(seq 1 "$ROUNDS"); do
  for i in $(seq 0 $((n - 1))); do
    idx=$(( (i + r - 1) % n ))
    run_one "$r" "${VARIANTS[$idx]}"
  done
  echo "[matrix] round $r/$ROUNDS done"
done
echo "[matrix] finished -> $OUT"
