#!/usr/bin/env bash
# Scenario: wire protocols through the TCP proxy (`sites[].tcp`) (the protocol guide, docs/protocols.md).
#
#   scripts/e2e/protocols.sh BINARY      (or through run.sh)
#
# One container runs small servers that answer the opening exchange of real protocols (payload/wire.py: Redis RESP, MQTT 3.1.1,
# PostgreSQL v3 with the SSL request, SMTP, the SSH identification strings, the RTMP handshake, FTP with a passive data
# connection) and an `openssl s_server`; Conduit has one TCP site per port, on the protocol's usual port number. Clients
# from the standard library (smtplib, ftplib) or written for the purpose talk to Conduit. The TLS check shows the proxy does not
# terminate TLS: the certificate the client verifies is the upstream's. Needs `--features tcp` in the binary.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
e2e_make_cert app.test backend.test
e2e_network

# The backend: every protocol server in one container. The FTP server tells clients to open the data connection to
# app.test:2122, which is Conduit's second FTP site (a passive data port has to be proxied as well).
docker run -d --name "conduit-e2e-backend-$E2E_ID" --network "$NET" --ip 192.0.2.61 --network-alias backend.test \
  -v "$here/payload:/payload:ro" -v "$WORK:/conf:ro" -e PASV_HOST=app.test -e PASV_PORT=2122 conduit-e2e-client sh -c '
    for spec in redis:6379 mqtt:1883 postgres:5432 smtp:25 ssh:22 rtmp:1935 ftp:21; do
      python3 /payload/wire.py server "${spec%%:*}" "${spec##*:}" &
    done
    openssl s_server -accept 8883 -cert /conf/cert.pem -key /conf/key.pem -www -quiet &
    wait' >/dev/null
E2E_WHO="conduit-e2e-backend-$E2E_ID"

{
  echo "sites:"
  for spec in 6379:6379 1883:1883 5432:5432 25:25 22:22 1935:1935 21:21 2122:2122 8883:8883; do
    echo "  - {port: ${spec%%:*}, tcp: {targets: [\"backend.test:${spec##*:}\"]}}"
  done
} > "$WORK/scenario.yaml"

cp "$WORK/scenario.yaml" "$WORK/conduit.yaml"
docker run -d --name "$APP" --network "$NET" --ip 192.0.2.20 --network-alias app.test \
  -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" conduit -c /conf/conduit.yaml >/dev/null
for _ in $(seq 1 60); do
  if client sh -c 'for p in 6379 1883 5432 25 22 1935 21 2122 8883; do nc -z -w 1 app.test $p && nc -z -w 1 backend.test $p || exit 1; done'; then up=1; break; fi
  sleep 0.5
done
[ -n "${up:-}" ] || fail "the TCP sites or the backend servers do not accept connections"
e2e_prove_isolation

wire() { # PROTOCOL PORT: a client of the protocol against Conduit, its JSON answer on stdout
  docker run --rm --network "$NET" -v "$here/payload:/payload:ro" conduit-e2e-client python3 /payload/wire.py client "$1" app.test "$2"
}
expect() { # NAME PORT JQ-FILTER
  local out
  out=$(wire "$1" "$2") || fail "$1: the client failed: $out"
  echo "$out" | jqe "$3" >/dev/null || fail "$1: unexpected answer: $out"
  ok "$1 through the TCP proxy on port $2: $out"
}

expect redis 6379 '.ping == "+PONG" and .set == "+OK" and .get == "bulk of 100000 bytes"'
expect mqtt 1883 '.connack == "20020000" and .puback == "4002002a" and .pingresp == "d000"'
expect postgres 5432 '.ssl_answer == "N" and .auth_ok and .server_version and .row'
expect smtp 25 '.ehlo == 250 and (.failed_recipients | length == 0)'
expect ssh 22 '.banner == "SSH-2.0-e2e_server" and .answer == "you said SSH-2.0-e2e_client"'
expect rtmp 1935 '.version == 3 and .s2_echoes_c1 and .done == "handshake-done"'
# FTP: the control connection on 21 and the passive data connection on 2122, both proxied
expect ftp 21 '.welcome == "220 ftp.test ready" and (.listing | length == 1) and (.listing[0] | endswith("hello.txt"))'

# TLS is not terminated: a client that verifies the upstream's certificate (SAN backend.test) connects through the proxy.
out=$(client sh -c 'printf "GET / HTTP/1.0\r\n\r\n" | openssl s_client -quiet -connect app.test:8883 -servername backend.test -CAfile /conf/cert.pem -verify_return_error -verify_hostname backend.test 2>&1')
echo "$out" | grep -q '200 ok' || fail "TLS through the proxy did not reach the upstream's TLS server: $out"
ok "TLS passes through untouched: a client verified the upstream's certificate for backend.test through the TCP proxy"

echo "protocols scenario passed"
