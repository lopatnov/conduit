#!/usr/bin/env bash
# Scenario: the guards and the request-handling limits, attacked with real clients (issue #704 and #705).
#
#   scripts/e2e/guards.sh BINARY      (or through run.sh)
#
# IP filter (and a spoofed X-Forwarded-For), rate limit (429), body and header limits (413, 431), CORS, security
# headers, redirects, error masking, and wire-level attacks that need a raw socket: request smuggling (CL.TE, TE.CL,
# duplicate Content-Length), header injection, path traversal through a `stripPrefix` route. Each guard answers what it
# should and the upstream never sees the attack.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
e2e_network
e2e_who_upstreams who1

U=http://app.test:8080
code() { client curl -sS --max-time 10 -o /dev/null -w '%{http_code}' "$@" 2>/dev/null || true; }
restart() {
  docker rm -f "$APP" >/dev/null 2>&1 || true
  e2e_start_conduit "$@"
}
# raw HOST_AND_PORT: send stdin to app.test:8080 over a plain socket, print the response (nc, 3 s)
raw() { docker run --rm -i --network "$NET" conduit-e2e-client sh -c 'nc -w 3 app.test 8080'; }
seen() { client curl -sS --max-time 10 http://who1.test:8080/seen; }

# ── 1. IP filter ──
cat > "$WORK/ip.yaml" <<'YAML'
port: 8080
host: app.test
ipFilter:
  allow: ["192.0.2.10/32", "203.0.113.7/32"]
proxy: "http://who1.test:8080"
YAML
docker rm -f "$APP" >/dev/null 2>&1 || true
cp "$WORK/ip.yaml" "$WORK/conduit.yaml"
docker run -d --name "$APP" --network "$NET" --ip 192.0.2.20 --network-alias app.test -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" conduit -c /conf/conduit.yaml >/dev/null
sleep 2
[ "$(code $U/)" = 403 ] || fail "ipFilter: a client outside the allowlist was served ($(code $U/))"
[ "$(code -H 'X-Forwarded-For: 203.0.113.7' $U/)" = 403 ] || fail "ipFilter: a spoofed X-Forwarded-For got a client past the allowlist"
[ "$(code -H 'X-Real-IP: 203.0.113.7' $U/)" = 403 ] || fail "ipFilter: a spoofed X-Real-IP got a client past the allowlist"
[ "$(code -H 'Forwarded: for=203.0.113.7' $U/)" = 403 ] || fail "ipFilter: a spoofed Forwarded header got a client past the allowlist"
ok "ipFilter: an address outside the allowlist is 403 and X-Forwarded-For / X-Real-IP / Forwarded do not change that"

# ── 2. Rate limit, request limits, CORS, security headers, redirects, error masking ──
cat > "$WORK/guards.yaml" <<'YAML'
port: 8080
host: app.test
rateLimit:
  windowSecs: 60
  limit: 20
  keyBy: ip
  skipPaths: [/__health__]
limits:
  maxBodyBytes: 1024
  maxRequestHeaders: 30
cors:
  origins: ["https://good.test"]
  methods: [GET, POST]
  allowedHeaders: [Content-Type]
securityHeaders:
  hstsMaxAgeSecs: 63072000
redirects:
  - from: /old
    to: "https://good.test/new"
    status: 301
maskErrors: true
healthCheck: true
proxy:
  /: "http://who1.test:8080"
  /api: { targets: ["http://who1.test:8080"], stripPrefix: true }
YAML
restart "$WORK/guards.yaml" $U/__health__

out=$(client curl -sSI --max-time 10 $U/)
echo "$out" | grep -qi '^x-content-type-options: nosniff' || fail "securityHeaders: no nosniff: $out"
echo "$out" | grep -qi '^x-frame-options:' || fail "securityHeaders: no X-Frame-Options"
# RFC 6797 section 7.2 (#723): no HSTS over plain HTTP, but yes behind a balancer that says it terminated TLS
echo "$out" | grep -qi '^strict-transport-security' && fail "securityHeaders: HSTS was sent over plain HTTP: $out"
out=$(client curl -sSI --max-time 10 -H 'X-Forwarded-Proto: https' $U/)
echo "$out" | grep -qi '^strict-transport-security: max-age=63072000' || fail "securityHeaders: no HSTS behind X-Forwarded-Proto: https: $out"
ok "securityHeaders: nosniff and X-Frame-Options are set; HSTS only when the request was secure (#723)"

out=$(client curl -sSI --max-time 10 -X OPTIONS -H 'Origin: https://good.test' -H 'Access-Control-Request-Method: POST' $U/)
echo "$out" | grep -qi '^access-control-allow-origin: https://good.test' || fail "CORS: a preflight from an allowed origin got no allow-origin: $out"
out=$(client curl -sSI --max-time 10 -X OPTIONS -H 'Origin: https://evil.test' -H 'Access-Control-Request-Method: POST' $U/)
echo "$out" | grep -qi '^access-control-allow-origin' && fail "CORS: a preflight from a foreign origin was allowed: $out"
out=$(client curl -sSI --max-time 10 -H 'Origin: null' $U/)
echo "$out" | grep -qi '^access-control-allow-origin' && fail "CORS: the null origin was allowed"
ok "CORS: the allowed origin gets the headers; a foreign origin and the null origin do not"

out=$(client curl -sSI --max-time 10 $U/old)
echo "$out" | head -1 | grep -q 301 && echo "$out" | grep -qi '^location: https://good.test/new' || fail "redirect: $out"
out=$(client curl -sSI --max-time 10 "$U/old?next=https://evil.test" )
echo "$out" | grep -qi '^location: https://good.test/' || fail "redirect: the query changed the destination: $out"
ok "redirects: the destination host is the configured one (the query is carried over, it does not choose the host)"

[ "$(code -X POST -H 'Content-Type: text/plain' --data "$(head -c 4096 /dev/zero | tr '\0' a)" $U/)" = 413 ] || fail "limits: a 4 KiB body over a 1 KiB limit was not 413"
# the same body sent chunked (no Content-Length) is refused too, not forwarded cut off (#724)
[ "$(code -X POST -H 'Transfer-Encoding: chunked' -H 'Content-Type: text/plain' --data-binary "$(head -c 4096 /dev/zero | tr '\0' a)" $U/)" = 413 ] || fail "limits: a chunked 4 KiB body over a 1 KiB limit was not 413"
hdrs=(); for i in $(seq 1 60); do hdrs+=(-H "X-H$i: v"); done
[ "$(code "${hdrs[@]}" $U/)" = 431 ] || fail "limits: 60 headers over a limit of 30 was $(code "${hdrs[@]}" $U/), expected 431"
ok "limits: 413 for a body over the limit (Content-Length and chunked), 431 for too many headers"

out=$(client curl -sS --max-time 10 $U/api/status/500)
echo "$out" | grep -q 'who1' && fail "maskErrors: the upstream's body reached the client: $out"
echo "$out" | jqe '.status == 500' >/dev/null || fail "maskErrors: not the generic error: $out"
ok "maskErrors: a 5xx body from the upstream is replaced"

# wire-level attacks: nothing may reach who1 as a second request, and the answer is an error or exactly one response
smuggle() { # smuggle NAME: stdin is the raw request; fails when the upstream saw a request it was not sent
  local out n
  out=$(raw)
  n=$(echo "$out" | grep -c '^HTTP/1')
  [ "$n" -le 1 ] || fail "$1: $n responses to one request: $out"
  seen | grep -q 'smuggled\|GPOST' && fail "$1: the upstream saw a smuggled request: $(seen)"
  echo "$out" | head -1 | tr -d '\r'
}
cl_te=$(printf 'POST / HTTP/1.1\r\nHost: app.test\r\nContent-Length: 6\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\nGET /api/smuggled HTTP/1.1\r\nHost: app.test\r\n\r\n')
st=$(printf '%s' "$cl_te" | smuggle "CL.TE"); ok "CL.TE: $st"
te_cl=$(printf 'POST / HTTP/1.1\r\nHost: app.test\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\n\r\n5c\r\nGPOST / HTTP/1.1\r\nContent-Length: 15\r\n\r\nx=1\r\n0\r\n\r\n')
st=$(printf '%s' "$te_cl" | smuggle "TE.CL"); ok "TE.CL: $st"
dup=$(printf 'POST / HTTP/1.1\r\nHost: app.test\r\nContent-Length: 4\r\nContent-Length: 40\r\n\r\nabcdGET /api/smuggled HTTP/1.1\r\nHost: app.test\r\n\r\n')
st=$(printf '%s' "$dup" | smuggle "duplicate Content-Length"); ok "duplicate Content-Length: $st"
obs=$(printf 'GET / HTTP/1.1\r\nHost: app.test\r\nTransfer-Encoding : chunked\r\nX-A: b\r\n c\r\n\r\n')
st=$(printf '%s' "$obs" | smuggle "obs-fold / space before colon"); ok "obs-fold and space before the colon: $st"
badhost=$(printf 'GET / HTTP/1.1\r\nHost: app.test\r\nHost: evil.test\r\n\r\n')
st=$(printf '%s' "$badhost" | smuggle "two Host headers"); ok "two Host headers: $st"

# header injection: a bare LF or CR in a value, a NUL, a request target with CRLF; the upstream must not see a new header
# a bare LF is a line terminator for the parser (RFC 9112 section 2.2 allows it), so `a\nX-Injected: 1` is two headers the
# client chose itself, as it could with CRLF: not an injection. A bare CR and a NUL must not get through.
for hv in 'X-Inj: a\rX-Injected: 1' 'X-Inj: a\0X-Injected: 1'; do
  out=$(printf "GET /headers HTTP/1.1\r\nHost: app.test\r\n$hv\r\nConnection: close\r\n\r\n" | raw)
  echo "$out" | grep -qi '"x-injected"' && fail "header injection through '$hv' made a header at the upstream: $out"
done
ok "header injection: a bare CR and a NUL in a value make no header at the upstream"

# path traversal through the stripPrefix route: the upstream must never be asked for a path outside /api's own tree
for p in '/api/../secret' '/api/%2e%2e/secret' '/api/..%2fsecret' '/api//..//secret' '/api/%252e%252e/secret'; do
  client curl -sS --max-time 10 -o /dev/null --path-as-is "$U$p" || true
done
seen | grep -q '^GET /secret' && fail "path traversal: the upstream was asked for /secret: $(seen)"
ok "path traversal: ../ in five encodings never reached the upstream as /secret"

# rate limit last, it uses up the budget: 20 per minute, health is exempt
n429=0; for i in $(seq 1 40); do [ "$(code $U/)" = 429 ] && n429=$((n429 + 1)); done
[ "$n429" -ge 15 ] || fail "rateLimit: only $n429 of 40 requests were 429 with a limit of 20"
[ "$(code $U/__health__)" = 200 ] || fail "rateLimit: the health path was limited"
client curl -sSI --max-time 10 $U/ | grep -qi '^retry-after:' || echo "note: the 429 carries no Retry-After header (RFC 6585 section 4 says it MAY)"
ok "rateLimit: $n429 of 40 were 429, the health path is exempt"

echo "guards scenario passed"
