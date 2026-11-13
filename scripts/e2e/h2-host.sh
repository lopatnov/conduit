#!/usr/bin/env bash
# Scenario: HTTP/2 and the request host (issue #704, PR #703).
#
#   scripts/e2e/h2-host.sh BINARY      (or through run.sh)
#
# Conduit with TLS, h2 and `allowedHosts` app.test and other.test proxies to go-httpbin. curl and nghttp (other
# implementations of the protocol than the in-process tests use) check that an h2 request for an allowed host is served
# and that the upstream sees the authority, that a name that is not allowed and a `host` that disagrees with `:authority`
# are refused, and that a client's X-Forwarded-Host and CRLF in the request target do not reach the upstream. The
# in-process tests of the same behaviour are in tests/h2_authority.rs and are not repeated.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
e2e_make_cert app.test other.test
cat > "$WORK/scenario.yaml" <<'EOF'
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
e2e_network
e2e_start_conduit "$WORK/scenario.yaml" https://app.test:8443/get
CURL=(curl -sS --max-time 10 --cacert /conf/cert.pem)

e2e_prove_isolation

# 1. h2: an allowed host is served, and the upstream sees the authority (the bug of PR #703).
out=$(client "${CURL[@]}" --http2 -w '\n%{http_code} %{http_version}' https://app.test:8443/headers)
[ "$(echo "$out" | tail -1)" = "200 2" ] || fail "h2 to app.test: $(echo "$out" | tail -1)"
echo "$out" | sed '$d' | jqe '.headers.Host == ["app.test:8443"] and .headers["X-Forwarded-Host"] == ["app.test:8443"]' >/dev/null \
  || fail "the upstream did not see app.test:8443 as Host and X-Forwarded-Host: $out"
ok "h2 request is served, the upstream sees the authority"

# 2. The same over HTTP/1.1.
out=$(client "${CURL[@]}" --http1.1 -w '\n%{http_code} %{http_version}' https://app.test:8443/headers)
[ "$(echo "$out" | tail -1)" = "200 1.1" ] || fail "HTTP/1.1 to app.test: $(echo "$out" | tail -1)"
ok "HTTP/1.1 request is served"

# 3. Another name the site allows is served; a name it does not allow is refused (curl sets :authority from the URL).
code=$(client "${CURL[@]}" --http2 -o /dev/null -w '%{http_code}' --connect-to other.test:8443:app.test:8443 https://other.test:8443/get)
[ "$code" = "200" ] || fail "h2 to other.test: $code"
code=$(client curl -sS --max-time 10 -k --http2 -o /dev/null -w '%{http_code}' --connect-to evil.test:8443:app.test:8443 https://evil.test:8443/get)
[ "$code" = "400" ] || fail "h2 to evil.test (not allowed) answered $code, expected 400"
code=$(client curl -sS --max-time 10 -k --http1.1 -o /dev/null -w '%{http_code}' --connect-to evil.test:8443:app.test:8443 https://evil.test:8443/get)
[ "$code" = "400" ] || fail "HTTP/1.1 to evil.test (not allowed) answered $code, expected 400"
ok "allowed names are served, a name that is not allowed is refused on h2 and HTTP/1.1"

# 4. Host header injection with nghttp, which can send both: `:authority` and `host` that disagree are refused even
# though both names are allowed (RFC 9113 section 8.3.1); the same name twice is served (the control).
status() { client nghttp -n -v "$@" 2>&1 | sed -n 's/.*:status: *\([0-9]*\).*/\1/p' | head -1; }
same=$(status -H':authority: app.test:8443' -H'host: app.test:8443' https://app.test:8443/get)
[ "$same" = "200" ] || fail "nghttp with the same authority and host answered '$same', expected 200"
mixed=$(status -H':authority: app.test:8443' -H'host: other.test' https://app.test:8443/get)
[ "$mixed" = "400" ] || fail "nghttp with :authority app.test and host other.test answered '$mixed', expected 400"
ok "a host that disagrees with :authority is refused"

# 5. Header spoofing: the client's X-Forwarded-Host is replaced, the upstream sees the real one.
out=$(client "${CURL[@]}" --http2 -H 'X-Forwarded-Host: evil.test' https://app.test:8443/headers)
echo "$out" | jqe '.headers["X-Forwarded-Host"] == ["app.test:8443"]' >/dev/null \
  || fail "the upstream saw the client's X-Forwarded-Host: $out"
ok "a client's X-Forwarded-Host does not reach the upstream"

# 6. CRLF injection through the request target does not make a header at the upstream.
out=$(client "${CURL[@]}" --http2 --path-as-is 'https://app.test:8443/headers?x=%0d%0aX-Injected:%201')
echo "$out" | jqe '.headers | has("X-Injected") | not' >/dev/null || fail "CRLF in the target made a header: $out"
ok "CRLF in the request target makes no header"

echo "h2-host scenario passed"
