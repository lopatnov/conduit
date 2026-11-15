#!/usr/bin/env bash
# Scenario: Server-Sent Events and WebSocket through Conduit, over plain HTTP and TLS (the protocol guide, docs/protocols.md).
#
#   scripts/e2e/streaming.sh BINARY      (or through run.sh)
#
# Two small upstreams in the network (payload/streaming.py, standard library only): an SSE server that sends events on a
# timer and can go quiet, and a WebSocket echo server. Conduit proxies both on a plain port and on a TLS port with
# compression on. payload/streaming_checks.py then checks, with clients written for the purpose: events arrive one by one and
# are not compressed, Last-Event-ID reaches the upstream, `timeout.readMs` ends an idle stream and an idle WebSocket (and
# pings or a larger value keep them), the WebSocket handshake, text, ping/pong, fragments, a 1 MiB binary message, the
# close handshake, the Origin and X-Forwarded-For the upstream sees, and a 502 where `websocket: true` is not set. curl
# adds the same SSE stream over HTTP/2.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
e2e_make_cert app.test
e2e_network

# The upstreams run the payload from the repository; the client container gets it at /payload.
for spec in "sse:9000:192.0.2.41" "ws:9001:192.0.2.42"; do
  IFS=: read -r name port ip <<< "$spec"
  docker run -d --name "conduit-e2e-stream-$name-$E2E_ID" --network "$NET" --ip "$ip" --network-alias "$name.test" \
    -v "$here/payload:/payload:ro" conduit-e2e-client python3 /payload/streaming.py "$name-server" "$port" >/dev/null
  E2E_WHO="${E2E_WHO:-} conduit-e2e-stream-$name-$E2E_ID"
done

# The routes of the two sites (a plain port and a TLS port). The two `quick-` routes have a read timeout of 2 s, the two
# `patient-` ones of 10 s, the others keep the default (30 s); `plain-ws` does not allow WebSocket upgrades.
routes() {
  cat <<'EOF'
    proxy:
      "/sse": {targets: ["http://sse.test:9000"], stripPrefix: true}
      "/quick-sse": {targets: ["http://sse.test:9000"], stripPrefix: true, timeout: {readMs: 2000}}
      "/patient-sse": {targets: ["http://sse.test:9000"], stripPrefix: true, timeout: {readMs: 10000}}
      "/ws": {targets: ["http://ws.test:9001"], stripPrefix: true, websocket: true}
      "/quick-ws": {targets: ["http://ws.test:9001"], stripPrefix: true, websocket: true, timeout: {readMs: 2000}}
      "/patient-ws": {targets: ["http://ws.test:9001"], stripPrefix: true, websocket: true, timeout: {readMs: 10000}}
      "/plain-ws": {targets: ["http://ws.test:9001"], stripPrefix: true}
EOF
}
{
  cat <<'EOF'
sites:
  - port: 8080
    host: app.test
    compression: true
EOF
  routes
  cat <<'EOF'
  - port: 8443
    host: app.test
    compression: true
    http2: {}
    tls:
      cert: /conf/cert.pem
      key: /conf/key.pem
EOF
  routes
} > "$WORK/scenario.yaml"

e2e_start_conduit "$WORK/scenario.yaml" https://app.test:8443/sse/echo-header
for _ in $(seq 1 40); do client sh -c 'nc -z -w 1 ws.test 9001' && up=1 && break; sleep 0.5; done
[ -n "${up:-}" ] || fail "the WebSocket upstream does not accept connections"
e2e_prove_isolation

# 1. The checks that need a WebSocket and an SSE client of their own.
docker run --rm --network "$NET" -v "$WORK:/conf:ro" -v "$here/payload:/payload:ro" -e CA_FILE=/conf/cert.pem \
  -w /payload conduit-e2e-client python3 streaming_checks.py http://app.test:8080 https://app.test:8443 \
  || fail "streaming_checks.py reported a failure (its last line above says which)"

# 2. The same SSE stream over HTTP/2: the first event comes out long before the stream ends (not held back by framing).
out=$(client curl -sS --max-time 20 --cacert /conf/cert.pem --http2 -N \
  -w '\n%{http_version} %{time_starttransfer} %{time_total}\n' 'https://app.test:8443/sse/events?n=3&gap=0.5')
stats=$(echo "$out" | tail -1)
set -- $stats
[ "$1" = "2" ] || fail "the SSE request did not use HTTP/2: $stats"
echo "$out" | grep -c '^data: tick' | grep -qx 3 || fail "the three events did not arrive over HTTP/2: $out"
awk -v first="$2" -v total="$3" 'BEGIN { exit !(first < 0.4 && total >= 1.0) }' \
  || fail "over HTTP/2 the first byte came at $2 s and the stream ended at $3 s: events were held back"
ok "SSE over HTTP/2: first event after ${2}s, stream ends after ${3}s"

echo "streaming scenario passed"
