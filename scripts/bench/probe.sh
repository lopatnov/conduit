#!/usr/bin/env bash
# Linux or WSL2. Issue #475: where does one proxied request's CPU time go — user space or the kernel, and in which threads?
# Compares a conduit build (1 worker, access log off) with the raw TCP relay (floor) under the same load.
#   probe.sh CONDUIT_BIN [ROUNDS]
set -uo pipefail
PERF=$HOME/perf
OHA=$PERF/tools/bin/oha
MOCK=$PERF/mock/target/release/bench-upstream
RELAY=$PERF/relay/target/release/relay
PP=$PERF/pp/target/release/pp
BIN=${1:?usage: probe.sh BIN [ROUNDS]}
ROUNDS=${2:-3}
CPU_PROXY=${CPU_PROXY:-4-11}
CPU_UP=${CPU_UP:-12-15}
CPU_LOAD=${CPU_LOAD:-16-21}
OUT=${OUT:-$PERF/probe.txt}
: > "$OUT"

wait_tcp() { for _ in $(seq 1 60); do (exec 3<>/dev/tcp/127.0.0.1/"$1") 2>/dev/null && return 0; sleep 0.25; done; return 1; }
snap() {  # pid -> "tid comm utime stime" lines (a thread name may contain spaces: split after the LAST ')')
  python3 - "$1" <<'PY'
import glob, sys
for path in glob.glob(f"/proc/{sys.argv[1]}/task/*/stat"):
    try:
        text = open(path).read()
    except OSError:
        continue
    tid = path.split("/")[-2]
    head, _, tail = text.rpartition(")")
    comm = head.split("(", 1)[1].replace(" ", "_")
    f = tail.split()
    print(tid, comm, f[11], f[12])   # utime, stime are fields 14 and 15 of the whole line = 12th and 13th after the state
PY
}

measure() {  # label
  local label=$1 pid=$2
  taskset -c "$CPU_LOAD" "$OHA" -z 4s -c 50 --no-tui http://127.0.0.1:8080/ > /dev/null 2>&1
  snap "$pid" > /tmp/pr-s0
  taskset -c "$CPU_LOAD" "$OHA" -z 10s -c 50 --no-tui --output-format json http://127.0.0.1:8080/ > /tmp/pr-oha.json 2>/dev/null
  snap "$pid" > /tmp/pr-s1
  python3 - "$label" >> "$OUT" <<'PY'
import json, sys
label = sys.argv[1]
m = json.load(open('/tmp/pr-oha.json'))['metrics']
rps = m["requests_per_sec"]; reqs = rps * 10
s0 = {l.split()[0]: l.split() for l in open('/tmp/pr-s0')}
s1 = {l.split()[0]: l.split() for l in open('/tmp/pr-s1')}
user = sys_ = 0; threads = []
for tid, a in s1.items():
    b = s0.get(tid, [tid, a[1], "0", "0"])
    du, ds = int(a[2]) - int(b[2]), int(a[3]) - int(b[3])
    user += du; sys_ += ds
    if du + ds > 0: threads.append((du + ds, a[1], du, ds))
threads.sort(reverse=True)
us = lambda ticks: ticks / 100.0 / reqs * 1e6
print(f"{label}: {rps:7.0f} req/s  p50 {m['latency_ms']['p50']:.2f} ms  cpu/req: user {us(user):5.1f} us + sys {us(sys_):5.1f} us = {us(user + sys_):5.1f} us")
for tot, name, du, ds in threads[:5]:
    print(f"    thread {name:16s} user {us(du):5.1f} us  sys {us(ds):5.1f} us  per request")
PY
}

for r in $(seq 1 "$ROUNDS"); do
  for target in ${TARGETS:-conduit pp relay}; do
    pkill -f bench-upstream 2>/dev/null; taskset -c "$CPU_UP" "$MOCK" & up=$!
    wait_tcp 4000 || { echo "mock not up" >&2; exit 1; }
    if [ "$target" = conduit ]; then
      printf '{ "global": { "workers": 1 }, "sites": [ { "port": 8080, "proxy": "http://127.0.0.1:4000", "logging": false } ] }' > /tmp/pr-conduit.json
      taskset -c "$CPU_PROXY" "$BIN" -c /tmp/pr-conduit.json > /dev/null 2>&1 & pid=$!
    elif [ "$target" = pp ]; then
      taskset -c "$CPU_PROXY" "$PP" > /dev/null 2>&1 & pid=$!
    else
      taskset -c "$CPU_PROXY" "$RELAY" > /dev/null 2>&1 & pid=$!
    fi
    wait_tcp 8080 || { echo "$target not up" >&2; kill -9 $pid $up 2>/dev/null; continue; }
    measure "round $r  $target" "$pid"
    echo "    threads alive after load: $(ls /proc/$pid/task | wc -l)" >> "$OUT"
    kill -9 $pid $up 2>/dev/null; wait $pid $up 2>/dev/null; sleep 2
  done
done
cat "$OUT"
