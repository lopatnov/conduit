#!/usr/bin/env bash
# Scenario: the global allocators (issue #486).
#
#   scripts/e2e/allocator-check.sh SYSTEM_BINARY MIMALLOC_BINARY JEMALLOC_BINARY
#
# Three builds of the same source: the system allocator (`--no-default-features --features proxy,compression,static,
# hotreload`), the default build (mimalloc) and `--features jemalloc`. Each allocator prints its
# statistics when the process exits if its own environment variable asks for it (mimalloc: MIMALLOC_SHOW_STATS; jemalloc
# with tikv's symbol prefix: _RJEM_MALLOC_CONF=stats_print:true), which shows the allocator in the binary is the one
# the features chose, and that the build with neither has none. Then each binary proxies one request to a local upstream,
# so an allocator that links but breaks the server cannot pass. Host-only (loopback, python3, curl); no Docker.
set -euo pipefail

[ "$#" -eq 3 ] || { echo "usage: $0 SYSTEM_BINARY MIMALLOC_BINARY JEMALLOC_BINARY" >&2; exit 2; }
SYSTEM=$(realpath "$1"); MIMALLOC=$(realpath "$2"); JEMALLOC=$(realpath "$3")
WORK=$(mktemp -d)
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true; done
  rm -rf "$WORK"
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }

mi_stats() { MIMALLOC_SHOW_STATS=1 "$1" --version 2>&1 | grep -c 'heap stats\|pages  *peak\|pages .*peak' || true; }
je_stats() { _RJEM_MALLOC_CONF=stats_print:true "$1" --version 2>&1 | grep -c 'Begin jemalloc statistics' || true; }

# 1. Which allocator is in which binary.
[ "$(mi_stats "$MIMALLOC")" -ge 1 ] || fail "the mimalloc build does not report mimalloc statistics"
[ "$(je_stats "$MIMALLOC")" -eq 0 ] || fail "the mimalloc build reports jemalloc statistics"
[ "$(je_stats "$JEMALLOC")" -ge 1 ] || fail "the jemalloc build does not report jemalloc statistics"
[ "$(mi_stats "$JEMALLOC")" -eq 0 ] || fail "the jemalloc build reports mimalloc statistics"
[ "$(mi_stats "$SYSTEM")" -eq 0 ] && [ "$(je_stats "$SYSTEM")" -eq 0 ] || fail "the system-allocator build reports a custom allocator"
ok "each binary carries the allocator it was built with; the system-allocator build carries neither"

# 2. Each binary proxies a request.
mkdir -p "$WORK/www"; echo hello-from-upstream > "$WORK/www/index.html"
python3 -m http.server 18486 --bind 127.0.0.1 --directory "$WORK/www" >/dev/null 2>&1 &
PIDS+=($!)
for _ in $(seq 1 40); do curl -sf http://127.0.0.1:18486/ >/dev/null 2>&1 && break; sleep 0.25; done

cat > "$WORK/conduit.yaml" <<'YAML'
port: 18487
host: 127.0.0.1
proxy: "http://127.0.0.1:18486"
YAML

for pair in "system:$SYSTEM" "mimalloc:$MIMALLOC" "jemalloc:$JEMALLOC"; do
  name=${pair%%:*}; bin=${pair#*:}
  "$bin" -c "$WORK/conduit.yaml" >"$WORK/$name.log" 2>&1 &
  pid=$!
  up=""
  for _ in $(seq 1 60); do
    if curl -sf --max-time 2 http://127.0.0.1:18487/ -o "$WORK/$name.out" 2>/dev/null; then up=1; break; fi
    kill -0 "$pid" 2>/dev/null || { cat "$WORK/$name.log" >&2; fail "$name: conduit exited"; }
    sleep 0.25
  done
  [ -n "$up" ] || { cat "$WORK/$name.log" >&2; fail "$name: conduit does not answer"; }
  grep -q hello-from-upstream "$WORK/$name.out" || fail "$name: the proxied body is wrong: $(cat "$WORK/$name.out")"
  # a few hundred more requests, so the allocator sees reuse and not only start-up
  for _ in $(seq 1 300); do curl -sf --max-time 5 http://127.0.0.1:18487/ -o /dev/null || fail "$name: a request failed"; done
  kill -9 "$pid"; wait "$pid" 2>/dev/null || true
  ok "$name build proxies requests"
done
