#!/usr/bin/env bash
# Scenario: the external processing filter against a processor built from Envoy's own protocol definitions (issue #27).
#
#   scripts/e2e/ext-proc.sh BINARY      (or through run.sh; BINARY needs `--features ext-proc`)
#
# The in-process tests of the filter (tests/ext_proc.rs, crates/conduit-ext-proc) talk to a processor written with the same
# hand-written messages as the client, so a misreading of the protocol shared by both would pass. Here the processor is
# scripts/e2e/ext_proc_fixture.py: the messages are the ones protoc generates from `envoy/service/ext_proc/v3/*.proto`
# (the `xds-protos` package), served by grpcio. Conduit, the processor and the upstream are separate processes on loopback;
# unlike the other scenarios of this directory there is no Docker network: nothing here needs one.
#
# Needs python3 with `venv` and pip access to PyPI (grpcio and xds-protos are installed into a temporary virtual environment),
# curl and jq. PROCESSOR_PYTHON=/path/to/python uses an interpreter that already has both packages instead.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
BIN=$(realpath "$1")
WORK=$(mktemp -d)
pids=()
cleanup() {
  status=$?
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  if [ "$status" -ne 0 ]; then
    for log in "$WORK"/*.log; do echo "--- $log"; tail -20 "$log" || true; done
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

command -v curl >/dev/null || fail "curl is needed"
command -v jq >/dev/null || fail "jq is needed"

PY=${PROCESSOR_PYTHON:-}
if [ -z "$PY" ]; then
  python3 -m venv "$WORK/venv"
  "$WORK/venv/bin/pip" install -q --disable-pip-version-check -r "$here/ext-proc-requirements.txt"
  PY="$WORK/venv/bin/python"
fi

free_port() { python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }
wait_for() { # URL
  for _ in $(seq 100); do
    [ "$(curl -s -o /dev/null -w '%{http_code}' --max-time 1 "$1" || true)" != 000 ] && return 0
    sleep 0.1
  done
  fail "nothing answered at $1"
}

UP_PORT=$(free_port)
PROC_PORT=$(free_port)
"$PY" "$here/ext_proc_fixture.py" upstream --port "$UP_PORT" >"$WORK/upstream.log" 2>&1 &
pids+=($!)
: >"$WORK/observed"
"$PY" "$here/ext_proc_fixture.py" processor --port "$PROC_PORT" --observed "$WORK/observed" >"$WORK/processor.log" 2>&1 &
PROC_PID=$!
pids+=("$PROC_PID")
for _ in $(seq 100); do grep -q listening "$WORK/processor.log" && break; sleep 0.1; done
grep -q listening "$WORK/processor.log" || fail "the processor did not start"

# conduit NAME ENTRY_CONFIG_JSON: a Conduit with one ext_proc entry in front of the upstream; the port is in $PORT_NAME.
start_conduit() {
  local name=$1 entry=$2 port admin
  port=$(free_port)
  admin=$(free_port)
  cat >"$WORK/$name.json" <<EOF
{
  "global": { "admin": { "bind": "127.0.0.1:$admin" } },
  "sites": [{
    "port": $port,
    "proxy": "http://127.0.0.1:$UP_PORT",
    "middleware": [{ "type": "ext_proc", "config": $entry }]
  }]
}
EOF
  "$BIN" -c "$WORK/$name.json" >"$WORK/$name.log" 2>&1 &
  pids+=($!)
  wait_for "http://127.0.0.1:$port/static"
  printf -v "PORT_$name" '%s' "$port"
}

GRPC="grpc://127.0.0.1:$PROC_PORT"
start_conduit buffered "{ \"grpc\": \"$GRPC\", \"timeoutMs\": 2000, \"requestBody\": \"buffered\", \"responsePhase\": true, \"responseBody\": \"observe\" }"
start_conduit streamed "{ \"grpc\": \"$GRPC\", \"timeoutMs\": 2000, \"requestBody\": \"streamed\" }"
start_conduit open "{ \"grpc\": \"$GRPC\", \"timeoutMs\": 500, \"failOpen\": true, \"requestBody\": \"buffered\" }"
B="http://127.0.0.1:$PORT_buffered"
S="http://127.0.0.1:$PORT_streamed"
O="http://127.0.0.1:$PORT_open"

# 1. Request headers: the processor's mutation reaches the upstream.
out=$(curl -sS --max-time 10 -H 'x-remove-me: 1' "$B/echo")
echo "$out" | jq -e '.headers["x-processed-by"] == "envoy-proto-processor" and (.headers | has("x-remove-me") | not)' >/dev/null \
  || fail "request header mutation not applied: $out"
ok "request headers: a header is set and another removed by a processor generated from Envoy's protos"

# 2. Request body, buffered: the processor's body replaces the client's, framed as chunked.
out=$(curl -sS --max-time 10 -X POST --data 'my secret key' "$B/echo")
echo "$out" | jq -e '.body == "my ****** key" and .headers["transfer-encoding"] == "chunked" and (.headers | has("content-length") | not)' >/dev/null \
  || fail "buffered body not rewritten: $out"
ok "buffered request body: rewritten by the processor, forwarded with chunked framing"

# 3. Request body, streamed.
out=$(curl -sS --max-time 10 -X POST --data 'a secret and another secret' "$S/echo")
echo "$out" | jq -e '.body == "a ****** and another ******"' >/dev/null || fail "streamed body not rewritten: $out"
ok "streamed request body: rewritten chunk by chunk"

# 4. An immediate response, from the headers and from a body.
code=$(curl -sS --max-time 10 -o "$WORK/body" -D "$WORK/head" -w '%{http_code}' "$B/deny")
[ "$code" = 403 ] || fail "/deny answered $code"
grep -qi '^x-by: envoy-proto-processor' "$WORK/head" || fail "the immediate response lost its header: $(cat "$WORK/head")"
[ "$(cat "$WORK/body")" = "denied by the processor" ] || fail "/deny body: $(cat "$WORK/body")"
code=$(curl -sS --max-time 10 -o "$WORK/body" -w '%{http_code}' -X POST --data 'an attack' "$B/echo")
[ "$code" = 403 ] && [ "$(cat "$WORK/body")" = "blocked by the processor" ] || fail "body denial: $code $(cat "$WORK/body")"
ok "immediate responses: 403 from the request headers and from a request body"

# 5. A request body over maxBodyBytes is refused with 413 before it reaches the processor.
start_conduit capped "{ \"grpc\": \"$GRPC\", \"timeoutMs\": 2000, \"requestBody\": \"buffered\", \"maxBodyBytes\": 8 }"
code=$(curl -sS --max-time 10 -o /dev/null -w '%{http_code}' -X POST --data '0123456789abcdef' "http://127.0.0.1:$PORT_capped/echo")
[ "$code" = 413 ] || fail "an over-sized body answered $code, expected 413"
ok "a buffered body over maxBodyBytes is refused with 413"

# 6. Response headers: changed by the processor (it is shown the upstream's :status), and a 500 replaced.
curl -sS --max-time 10 -D "$WORK/head" -o "$WORK/body" "$B/static"
grep -qi '^x-processor-saw-status: 200' "$WORK/head" || fail "the processor was not shown :status 200: $(cat "$WORK/head")"
grep -qi '^x-upstream-secret' "$WORK/head" && fail "x-upstream-secret was not removed: $(cat "$WORK/head")"
grep -qi '^x-keep: 1' "$WORK/head" || fail "an untouched upstream header is gone: $(cat "$WORK/head")"
[ "$(cat "$WORK/body")" = "upstream says hi" ] || fail "the response body changed: $(cat "$WORK/body")"
code=$(curl -sS --max-time 10 -o "$WORK/body" -w '%{http_code}' "$B/fail")
[ "$code" = 503 ] && grep -q 'masked by the processor' "$WORK/body" || fail "a 500 was not replaced: $code $(cat "$WORK/body")"
ok "response headers: changed, and a 500 replaced by the processor's own response"

# 7. Response body, observed: the processor is sent a copy and the client gets the body unchanged.
# The observation of the earlier /static response runs in the background: let it reach the file before the reset.
for _ in $(seq 100); do [ "$(cat "$WORK/observed")" = "upstream says hi" ] && break; sleep 0.1; done
: >"$WORK/observed"
curl -sS --max-time 10 -o "$WORK/body" "$B/static"
for _ in $(seq 100); do [ "$(cat "$WORK/observed")" = "upstream says hi" ] && break; sleep 0.1; done
[ "$(cat "$WORK/observed")" = "upstream says hi" ] || fail "the processor was shown: '$(cat "$WORK/observed")'"
[ "$(cat "$WORK/body")" = "upstream says hi" ] || fail "the client's body changed: $(cat "$WORK/body")"
ok "response body: the processor sees a copy, the client gets it unchanged"

# 8. The processor goes away: fail-closed by default, the request continues with failOpen.
kill "$PROC_PID"
wait "$PROC_PID" 2>/dev/null || true
code=$(curl -sS --max-time 10 -o /dev/null -w '%{http_code}' "$B/echo")
[ "$code" = 503 ] || fail "without a processor the default answered $code, expected 503"
out=$(curl -sS --max-time 10 -X POST --data 'kept as it was' "$O/echo")
echo "$out" | jq -e '.body == "kept as it was"' >/dev/null || fail "failOpen did not forward the request: $out"
ok "no processor: refused with 503 by default, forwarded unchanged with failOpen"
