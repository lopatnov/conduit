#!/usr/bin/env bash
# HTTP/2 and Host-header tests with real clients and a real upstream, in a Docker network that has no route to the
# Internet (issue #704, PR #703).
#
#   scripts/test-h2-docker.sh BINARY               BINARY: a conduit built with `--features standard` or more
#   CONDUIT_IMAGE=debian:13 scripts/test-h2-docker.sh BINARY
#                                                  the image the binary runs in (default: ubuntu:24.04 by digest; the
#                                                  binary of the CI job is built on its glibc)
#
# What is in the network (192.0.2.0/24 is TEST-NET-1, RFC 5737; `.test` names are reserved for tests, RFC 6761):
#   upstream.test   192.0.2.10   go-httpbin (pinned by digest): echoes the headers it received
#   app.test        192.0.2.20   the conduit under test: TLS with a certificate for app.test and other.test, h2,
#                                `allowedHosts` app.test and other.test, proxying to upstream.test
#   (the clients) curl and nghttp from a throw-away container on the same network
# The network is created with `--internal`, and the script proves it: a client cannot reach the Internet.
#
# The checks are the ones an in-process test cannot make with another implementation of the protocol (the in-process
# ones are in tests/h2_authority.rs and are not repeated): curl and nghttp talking h2 to conduit, a third-party upstream,
# the headers as the upstream saw them.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 BINARY" >&2
  exit 2
fi
BIN=$(realpath "$1")
CONDUIT_IMAGE="${CONDUIT_IMAGE:-ubuntu@sha256:2198e4c65076e9cb0b3d695219df667c0e3b0072da64ca3c2648aca17dcecaf5}"
HTTPBIN=mccutchen/go-httpbin@sha256:3992f3763e9ce5a4307eae0a869a78b4df3931dc8feba74ab823dd2444af6a6b
ID=$$
NET="conduit-h2-test-$ID"
APP="conduit-h2-app-$ID"
UP="conduit-h2-upstream-$ID"
WORK=$(mktemp -d)

cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "--- conduit log"
    docker logs "$APP" 2>&1 | tail -30 || true
  fi
  docker rm -f "$APP" "$UP" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# The client image is built first: it needs the Internet, the network below does not have it.
docker build -q -t conduit-h2-client - >/dev/null <<'EOF'
FROM debian@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends curl ca-certificates nghttp2-client jq >/dev/null \
 && rm -rf /var/lib/apt/lists/*
EOF
docker pull -q "$HTTPBIN" >/dev/null

# A certificate for the two names the site accepts; the clients verify it (curl --cacert).
openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj "/CN=app.test" \
  -addext "subjectAltName=DNS:app.test,DNS:other.test" \
  -keyout "$WORK/key.pem" -out "$WORK/cert.pem" 2>/dev/null
chmod 644 "$WORK/key.pem"
cat > "$WORK/conduit.yaml" <<'EOF'
port: 8443
host: app.test
tls:
  cert: /conf/cert.pem
  key: /conf/key.pem
http2:
  maxConcurrentStreams: 100
securityHeaders:
  allowedHosts:
    - app.test
    - other.test
proxy: "http://upstream.test:8080"
EOF

docker network create --internal --subnet 192.0.2.0/24 "$NET" >/dev/null
docker run -d --name "$UP" --network "$NET" --ip 192.0.2.10 --network-alias upstream.test "$HTTPBIN" >/dev/null
docker run -d --name "$APP" --network "$NET" --ip 192.0.2.20 --network-alias app.test \
  -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" \
  conduit -c /conf/conduit.yaml >/dev/null

# A throw-away client on the same network, with the certificate to verify against.
client() { docker run --rm --network "$NET" -v "$WORK:/conf:ro" conduit-h2-client "$@"; }
CURL=(curl -sS --max-time 10 --cacert /conf/cert.pem)
# jq runs in the client image too: the host may not have it.
jqe() { docker run --rm -i conduit-h2-client jq -e "$@"; }

# Wait until conduit answers (not a fixed sleep).
up=
for _ in $(seq 1 50); do
  if client "${CURL[@]}" -o /dev/null https://app.test:8443/get 2>/dev/null; then up=1; break; fi
  sleep 0.5
done
[ -n "$up" ] || { echo "conduit does not answer"; exit 1; }

fail() { echo "FAIL: $*" >&2; exit 1; }

# 0. The network is isolated: a client cannot reach the Internet.
if client curl -sS --max-time 4 -o /dev/null https://example.com 2>/dev/null; then
  fail "the test network has a route to the Internet"
fi
echo "ok: isolated network"

# 1. h2: an allowed host is served, and the upstream sees the authority (the bug of PR #703).
out=$(client "${CURL[@]}" --http2 -w '\n%{http_code} %{http_version}' https://app.test:8443/headers)
[ "$(echo "$out" | tail -1)" = "200 2" ] || fail "h2 to app.test: $(echo "$out" | tail -1)"
echo "$out" | sed '$d' | jqe '.headers.Host == ["app.test:8443"] and .headers["X-Forwarded-Host"] == ["app.test:8443"]' >/dev/null \
  || fail "the upstream did not see app.test:8443 as Host and X-Forwarded-Host: $out"
echo "ok: h2 request is served, the upstream sees the authority"

# 2. The same over HTTP/1.1.
out=$(client "${CURL[@]}" --http1.1 -w '\n%{http_code} %{http_version}' https://app.test:8443/headers)
[ "$(echo "$out" | tail -1)" = "200 1.1" ] || fail "HTTP/1.1 to app.test: $(echo "$out" | tail -1)"
echo "ok: HTTP/1.1 request is served"

# 3. Another name the site allows is served; a name it does not allow is refused (curl sets :authority from the URL).
code=$(client "${CURL[@]}" --http2 -o /dev/null -w '%{http_code}' --connect-to other.test:8443:app.test:8443 https://other.test:8443/get)
[ "$code" = "200" ] || fail "h2 to other.test: $code"
code=$(client curl -sS --max-time 10 -k --http2 -o /dev/null -w '%{http_code}' --connect-to evil.test:8443:app.test:8443 https://evil.test:8443/get)
[ "$code" = "400" ] || fail "h2 to evil.test (not allowed) answered $code, expected 400"
code=$(client curl -sS --max-time 10 -k --http1.1 -o /dev/null -w '%{http_code}' --connect-to evil.test:8443:app.test:8443 https://evil.test:8443/get)
[ "$code" = "400" ] || fail "HTTP/1.1 to evil.test (not allowed) answered $code, expected 400"
echo "ok: allowed names are served, a name that is not allowed is refused on h2 and HTTP/1.1"

# 4. Host header injection with nghttp, which can send both: `:authority` and `host` that disagree are refused even
# though both names are allowed (RFC 9113 section 8.3.1); the same name twice is served (the control).
status() { client nghttp -n -v "$@" 2>&1 | sed -n 's/.*:status: *\([0-9]*\).*/\1/p' | head -1; }
same=$(status -H':authority: app.test:8443' -H'host: app.test:8443' https://app.test:8443/get)
[ "$same" = "200" ] || fail "nghttp with the same authority and host answered '$same', expected 200"
mixed=$(status -H':authority: app.test:8443' -H'host: other.test' https://app.test:8443/get)
[ "$mixed" = "400" ] || fail "nghttp with :authority app.test and host other.test answered '$mixed', expected 400"
echo "ok: a host that disagrees with :authority is refused"

# 5. Header spoofing: the client's X-Forwarded-Host is replaced, the upstream sees the real one.
out=$(client "${CURL[@]}" --http2 -H 'X-Forwarded-Host: evil.test' https://app.test:8443/headers)
echo "$out" | jqe '.headers["X-Forwarded-Host"] == ["app.test:8443"]' >/dev/null \
  || fail "the upstream saw the client's X-Forwarded-Host: $out"
echo "ok: a client's X-Forwarded-Host does not reach the upstream"

# 6. CRLF injection through the request target does not make a header at the upstream.
out=$(client "${CURL[@]}" --http2 --path-as-is 'https://app.test:8443/headers?x=%0d%0aX-Injected:%201')
echo "$out" | jqe '.headers | has("X-Injected") | not' >/dev/null || fail "CRLF in the target made a header: $out"
echo "ok: CRLF in the request target makes no header"

echo "h2 docker tests passed"
