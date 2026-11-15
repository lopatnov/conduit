#!/usr/bin/env bash
# Scenario: gRPC through Conduit (the protocol guide, docs/protocols.md).
#
#   scripts/e2e/grpc.sh BINARY      (or through run.sh)
#
# A gRPC server without generated code (payload/grpc_echo.py, python3-grpcio) runs in the network once in cleartext and
# once with TLS. Four Conduit sites reach them: h2c downstream to a cleartext upstream with `http2: h2c`, the same through
# a TLS port (ALPN h2), h2c downstream to a TLS upstream with `http2: true`, and, as the control, a site whose route says
# `http2: true` for a cleartext upstream (which Conduit then speaks HTTP/1.1 to, so gRPC cannot work). The calls are
# unary with trailing metadata, server streaming, client streaming of 1 MB, an error status with its message and trailers,
# and a deadline (payload/grpc_checks.py).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
e2e_make_cert app.test grpcs.test
e2e_network

start_server() { # NAME IP ARGS...
  local name=$1 ip=$2
  shift 2
  docker run -d --name "conduit-e2e-grpc-$name-$E2E_ID" --network "$NET" --ip "$ip" --network-alias "$name.test" \
    -v "$here/payload:/payload:ro" -v "$WORK:/conf:ro" conduit-e2e-client python3 /payload/grpc_echo.py server "$@" >/dev/null
  E2E_WHO="${E2E_WHO:-} conduit-e2e-grpc-$name-$E2E_ID"
}
start_server grpc 192.0.2.51 50051
start_server grpcb 192.0.2.52 50051
start_server grpcs 192.0.2.53 50052 --tls /conf/cert.pem /conf/key.pem

cat > "$WORK/scenario.yaml" <<'YAML'
sites:
  - port: 8081
    host: app.test
    http2: {h2c: true}
    proxy:
      "/": {targets: ["http://grpc.test:50051"], http2: h2c}
  - port: 8082
    host: app.test
    http2: {h2c: true}
    proxy:
      "/": {targets: ["http://grpcb.test:50051"], http2: true}
  - port: 8083
    host: app.test
    http2: {h2c: true}
    proxy:
      "/": {targets: ["https://grpcs.test:50052"], http2: true, upstreamTls: {verify: false}}
  - port: 8443
    host: app.test
    http2: {}
    tls:
      cert: /conf/cert.pem
      key: /conf/key.pem
    proxy:
      "/": {targets: ["http://grpc.test:50051"], http2: h2c}
YAML
# The Conduit image has no CA bundle, and Conduit (Pingora) needs a non-empty root store to build its verifier even when a
# route sets `upstreamTls.verify: false`: without one every HTTPS upstream answers 502 ("no root trust anchors were provided").
E2E_CONDUIT_DOCKER_ARGS="-e SSL_CERT_FILE=/conf/cert.pem"
e2e_start_conduit "$WORK/scenario.yaml" https://app.test:8443/
for _ in $(seq 1 40); do client sh -c 'nc -z -w 1 grpc.test 50051 && nc -z -w 1 grpcb.test 50051 && nc -z -w 1 grpcs.test 50052' && up=1 && break; sleep 0.5; done
[ -n "${up:-}" ] || fail "the gRPC servers do not accept connections"
e2e_prove_isolation

checks() { # ARGS...: grpc_checks.py in the client image
  docker run --rm --network "$NET" -v "$WORK:/conf:ro" -v "$here/payload:/payload:ro" -w /payload conduit-e2e-client \
    python3 grpc_checks.py "$@"
}
checks works app.test:8081 || fail "gRPC over h2c to a cleartext upstream (http2: h2c)"
checks works app.test:8083 || fail "gRPC over h2c to a TLS upstream (http2: true)"
checks works app.test:8443 --ca /conf/cert.pem || fail "gRPC over TLS (ALPN h2) to a cleartext upstream (http2: h2c)"
checks refused app.test:8082 --direct grpcb.test:50051 || fail "the control: http2: true to a cleartext gRPC upstream must not work"

echo "grpc scenario passed"
