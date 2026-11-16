#!/usr/bin/env bash
# Scenario: TLS termination on TCP sites (`sites[].tls` next to `sites[].tcp`, issue #757), with real TLS clients.
#
#   scripts/e2e/tcp-tls.sh BINARY      (or through run.sh)
#
# One line-echo server (greets with `echo`, then echoes everything) that speaks no TLS, behind three Conduit TCP sites:
#   9400  tls                        the client verifies Conduit's certificate; the upstream gets plaintext; a client that
#                                    does not speak TLS is closed
#   9401  tls + clientAuth           a client without a certificate, or with one from another CA, never reaches the upstream
#   9402  tls + proxyProtocol        the header comes first, the TLS handshake second; the upstream is told the client of the
#         + sendProxyProtocol v1     header; a connection without the header gets no TLS
# Needs `--features tcp` in the binary.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client

# ── Certificates: a CA, the server (app.test), a client of that CA (alice) and a client of another CA (mallory) ──
mkca() { # mkca NAME
  openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj "/CN=$1" -keyout "$WORK/$1.key" -out "$WORK/$1.pem" 2>/dev/null
}
mkleaf() { # mkleaf CA NAME EXT   (EXT: serverAuth or clientAuth)
  openssl req -newkey rsa:2048 -nodes -subj "/CN=$2" -keyout "$WORK/$2.key" -out "$WORK/$2.csr" 2>/dev/null
  printf 'subjectAltName=DNS:%s\nextendedKeyUsage=%s\nbasicConstraints=CA:FALSE\n' "$2" "$3" > "$WORK/$2.ext"
  openssl x509 -req -in "$WORK/$2.csr" -CA "$WORK/$1.pem" -CAkey "$WORK/$1.key" -CAcreateserial -days 30 \
    -extfile "$WORK/$2.ext" -out "$WORK/$2.pem" 2>/dev/null
}
mkca e2e-ca
mkca rogue-ca
mkleaf e2e-ca app.test serverAuth
mkleaf e2e-ca alice clientAuth
mkleaf rogue-ca mallory clientAuth
chmod 644 "$WORK"/*.key
e2e_network

docker run -d --name "conduit-e2e-tcptls-echo-$E2E_ID" --network "$NET" --ip 192.0.2.31 --network-alias echo.test \
  conduit-e2e-client python3 -c '
import socketserver
class H(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.sendall(b"echo\n")
        while True:
            d = self.request.recv(65536)
            if not d:
                break
            self.request.sendall(d)
class S(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
S(("0.0.0.0", 9100), H).serve_forever()
' >/dev/null
E2E_WHO="${E2E_WHO:-} conduit-e2e-tcptls-echo-$E2E_ID"
sleep 1

cat > "$WORK/conduit.yaml" <<'YAML'
sites:
  - port: 9400
    tls:
      cert: /conf/app.test.pem
      key: /conf/app.test.key
    tcp:
      targets: ["echo.test:9100"]
  - port: 9401
    tls:
      cert: /conf/app.test.pem
      key: /conf/app.test.key
      clientAuth:
        ca: /conf/e2e-ca.pem
    tcp:
      targets: ["echo.test:9100"]
  - port: 9402
    tls:
      cert: /conf/app.test.pem
      key: /conf/app.test.key
    proxyProtocol:
      trustedSources: ["192.0.2.0/24"]
      timeoutMs: 2000
    tcp:
      targets: ["echo.test:9100"]
      sendProxyProtocol: v1
YAML
docker rm -f "$APP" >/dev/null 2>&1 || true
docker run -d --name "$APP" --network "$NET" --ip 192.0.2.20 --network-alias app.test \
  -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" conduit -c /conf/conduit.yaml >/dev/null
for _ in $(seq 1 50); do
  if client sh -c 'nc -z -w 1 app.test 9400'; then up=1; break; fi
  sleep 0.5
done
[ -n "${up:-}" ] || fail "the TCP proxy does not accept connections on 9400"
e2e_prove_isolation

# probe.py PORT IDENTITY PREAMBLE: connect to app.test:PORT, send PREAMBLE and a CRLF first when it is not "-", then a TLS
# handshake that verifies the server against the CA (presenting the certificate IDENTITY when it is not "-"), read the
# upstream's greeting, send `hi` and read until it comes back. Prints `served TEXT` (line ends shown as |), `refused KIND`
# when the server ended the connection or the handshake, and `timeout` when it did neither (which is never a refusal).
cat > "$WORK/probe.py" <<'PY'
import socket, ssl, sys
port, identity, preamble = int(sys.argv[1]), sys.argv[2], sys.argv[3]
ctx = ssl.create_default_context(cafile="/conf/e2e-ca.pem")
if identity != "-":
    ctx.load_cert_chain("/conf/%s.pem" % identity, "/conf/%s.key" % identity)
raw = socket.create_connection(("app.test", port), timeout=5)
try:
    if preamble != "-":
        raw.sendall(preamble.encode() + b"\r\n")
    tls = ctx.wrap_socket(raw, server_hostname="app.test")
    tls.settimeout(5)
    seen = tls.recv(16)  # with TLS 1.3 a refused client certificate shows up on the first read
    tls.sendall(b"hi\n")
    while b"hi\n" not in seen:
        chunk = tls.recv(4096)
        if not chunk:
            break
        seen += chunk
    print("served", seen.decode(errors="replace").replace("\r\n", "|").replace("\n", "|"))
except (ssl.SSLError, ConnectionError, EOFError) as e:
    print("refused", type(e).__name__)
except socket.timeout:
    print("timeout")
PY

# 1. 9400: the handshake is Conduit's; the upstream (which speaks no TLS) answers in plaintext
got=$(client python3 /conf/probe.py 9400 - - | tr -d '\r')
[ "$got" = "served echo|hi|" ] || fail "tls on a tcp site: expected 'served echo|hi|', got '$got'"
ok "9400: the client verified Conduit's certificate and the plaintext upstream answered through it ($got)"
plain=$(client python3 -c '
import socket
s = socket.create_connection(("app.test", 9400), timeout=5)
s.sendall(b"hi\n")
data = b""
try:
    while True:
        chunk = s.recv(4096)
        if not chunk:
            print("closed", b"echo" in data)
            break
        data += chunk
except ConnectionError:
    print("closed", b"echo" in data)
except socket.timeout:
    print("open", b"echo" in data)
' | tr -d '\r')
[ "$plain" = "closed False" ] || fail "a client that does not speak TLS: expected 'closed False', got '$plain'"
ok "9400: a client that does not speak TLS is closed and never reaches the upstream ($plain)"

# 2. 9401: clientAuth
got=$(client python3 /conf/probe.py 9401 alice - | tr -d '\r')
[ "$got" = "served echo|hi|" ] || fail "clientAuth, client certificate of the CA: expected 'served echo|hi|', got '$got'"
ok "9401: a client with a certificate of the CA is served ($got)"
for who in - mallory; do
  got=$(client python3 /conf/probe.py 9401 "$who" - | tr -d '\r')
  case "$got" in
    refused*) ok "9401: ${who/#-/no certificate} is refused ($got)" ;;
    *) fail "clientAuth, identity '$who': expected a refusal, got '$got'" ;;
  esac
done

# 3. 9402: the header is read before the handshake, and its client reaches the upstream
got=$(client python3 /conf/probe.py 9402 - "PROXY TCP4 198.51.100.9 192.0.2.20 51000 9402" | tr -d '\r')
case "$got" in
  "served echo|PROXY TCP4 198.51.100.9 192.0.2.20 51000 9402|hi|") ok "9402: header, then TLS: the upstream was told the client 198.51.100.9 ($got)" ;;
  *) fail "proxyProtocol with tls: expected the upstream to echo 'PROXY TCP4 198.51.100.9 192.0.2.20 51000 9402' before hi, got '$got'" ;;
esac
got=$(client python3 /conf/probe.py 9402 - - | tr -d '\r')
case "$got" in
  refused*) ok "9402: a connection without the header gets no TLS ($got)" ;;
  *) fail "proxyProtocol with tls, no header: expected a refusal, got '$got'" ;;
esac

echo "tcp-tls scenario passed"
