#!/usr/bin/env bash
# Scenario: WebDAV through Conduit (the protocol guide, docs/protocols.md).
#
#   scripts/e2e/webdav.sh BINARY      (or through run.sh)
#
# A WebDAV-shaped upstream (payload/webdav_server.py, standard library only) answers every method with the status a DAV
# server gives and echoes what it received. Conduit proxies it under `/dav` (stripPrefix), `/keep` (the prefix stays) and
# `/rw` (a rewrite rule), and, with `rewriteDestination: true`, `/mv` (stripPrefix) and `/rwd` (a rewrite rule), and serves a static directory on a second port. payload/webdav_checks.py then checks that
# PROPFIND, PROPPATCH, MKCOL, COPY, MOVE, LOCK, UNLOCK, DELETE, PUT and OPTIONS reach the upstream with their bodies and
# with Depth, Overwrite, Timeout, Lock-Token and Content-Type intact, that the response headers DAV, Allow, Lock-Token and
# Location and the hrefs of a multistatus body come back as the upstream wrote them, that the client's Host is kept, that
# `Destination` and the resource tags of `If` reach the upstream exactly as sent (not rewritten, whatever the route does to
# the request line) unless the route sets `rewriteDestination`, which maps them like the request path, and that a static site answers every method but GET, HEAD and OPTIONS with 405 and Allow.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
e2e_make_cert app.test
e2e_network

mkdir -p "$WORK/static"
printf 'hello\n' > "$WORK/static/hello.txt"
docker run -d --name "conduit-e2e-dav-$E2E_ID" --network "$NET" --ip 192.0.2.43 --network-alias dav.test \
  -v "$here/payload:/payload:ro" conduit-e2e-client python3 /payload/webdav_server.py 9100 >/dev/null
E2E_WHO="${E2E_WHO:-} conduit-e2e-dav-$E2E_ID"

cat > "$WORK/scenario.yaml" <<'YAML'
sites:
  - port: 8080
    host: app.test
    proxy:
      "/dav": {targets: ["http://dav.test:9100"], stripPrefix: true}
      "/keep": {targets: ["http://dav.test:9100"]}
      "/rw": {targets: ["http://dav.test:9100"], rewrite: [{from: "^/rw/", to: "/store/"}]}
      "/mv": {targets: ["http://dav.test:9100"], stripPrefix: true, rewriteDestination: true}
      "/rwd": {targets: ["http://dav.test:9100"], rewrite: [{from: "^/rwd/", to: "/store/"}], rewriteDestination: true}
  - port: 8081
    host: app.test
    static: /conf/static
YAML
# Conduit is up when the static site answers; the upstream is waited for below.
e2e_start_conduit "$WORK/scenario.yaml" http://app.test:8081/hello.txt
up=""
for _ in $(seq 1 40); do client sh -c 'nc -z -w 1 dav.test 9100' && up=1 && break; sleep 0.5; done
[ -n "$up" ] || fail "the WebDAV upstream does not accept connections"

docker run --rm --network "$NET" -v "$here/payload:/payload:ro" -w /payload conduit-e2e-client \
  python3 webdav_checks.py http://app.test:8080 http://app.test:8081 \
  || fail "webdav_checks.py reported a failure (its last line above says which)"

echo "webdav scenario passed"
