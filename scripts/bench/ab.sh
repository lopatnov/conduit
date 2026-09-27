#!/usr/bin/env bash
# A/B of two Conduit binaries on the throughput matrix: which one serves more requests per second, and at what CPU cost
# per request? Alternates the arms ABBA within every round (a drift or a first-run penalty then hits both alike).
#
#   scripts/bench/ab.sh BIN_A BIN_B [ROUNDS]        ONLY="w1-nolog w4-nolog" (variants of matrix.sh), default below
#
# Prints, per variant and arm, the mean requests/s, p50/p99 and CPU µs per request, and every run. Layout and prerequisites:
# scripts/bench/README.md. Do not run other CPU-heavy work while it measures.
set -uo pipefail
A=${1:?usage: ab.sh BIN_A BIN_B [ROUNDS]}
B=${2:?usage: ab.sh BIN_A BIN_B [ROUNDS]}
ROUNDS=${3:-3}
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
OUT=${OUT:-/tmp/ab.csv}
ONLY=${ONLY:-"w1-nolog w2-nolog w4-nolog w8-nolog"}

echo "arm,round,variant,workers,connections,rps,p50_ms,p99_ms,ok_pct,cpu_us_per_req" > "$OUT"
for r in $(seq 1 "$ROUNDS"); do
  for arm in A B B A; do
    bin=$A; [ "$arm" = B ] && bin=$B
    ONLY="$ONLY" bash "$HERE/matrix.sh" "$bin" /tmp/ab-one.csv 1 > /dev/null 2>&1
    tail -n +2 /tmp/ab-one.csv | sed "s/^/$arm,/" >> "$OUT"
  done
  echo "[ab] round $r/$ROUNDS done"
done
python3 - "$OUT" <<'PY'
import csv, collections, sys
rows = list(csv.DictReader(open(sys.argv[1])))
by = collections.OrderedDict()
for r in rows:
    by.setdefault((r["variant"], r["arm"]), []).append(r)
print("%-10s %-4s %9s %8s %8s %9s  %s" % ("variant", "arm", "rps", "p50 ms", "p99 ms", "cpu us", "runs"))
for (v, a), rs in sorted(by.items()):
    f = lambda k: sum(float(x[k]) for x in rs) / len(rs)
    print("%-10s %-4s %9.0f %8.2f %8.2f %9.1f  %s" % (v, a, f("rps"), f("p50_ms"), f("p99_ms"), f("cpu_us_per_req"), ",".join(x["rps"] for x in rs)))
PY
