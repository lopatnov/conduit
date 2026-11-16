#!/usr/bin/env bash
# Scenario: the PROXY protocol with a real load balancer in front (`sites[].proxyProtocol`, `tcp.sendProxyProtocol`;
# issue #757). HAProxy (pinned by digest) sits between the clients and Conduit and sends v2 (and v1) headers.
#
#   scripts/e2e/proxy-protocol.sh BINARY      (or through run.sh)
#
# Clients have fixed addresses (192.0.2.60 and .61), the load balancer is 192.0.2.50, Conduit 192.0.2.20:
#   HTTPS  8443  proxyProtocol from 192.0.2.50 only; ipFilter allows 192.0.2.60 only, the load balancer's own address is
#                not on it: a 200 for .60 and a 403 for .61 through the same HAProxy show that the address in the header
#                is the one the filter sees, and X-Forwarded-For carries it. A client that skips the load balancer is not
#                a trusted source and gets no TLS.
#   tcp    9300  accepts the header and sends v2 to the upstream, which reports the client it was told
#   tcp    9301  accepts the header and sends none: the upstream sees a plain connection (the header is consumed)
#   tcp    9302  maxConnectionsPerIp: 1 counts the client of the header, not the load balancer
# Needs `--features tcp` in the binary.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

# HAProxy 3.0 LTS (alpine), the multi-arch index digest
HAPROXY=haproxy@sha256:d2b3567d836f3b7d174edb6d6f611ed5308e7966e53a9cd9a597e0415a298efd

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
docker pull -q "$HAPROXY" >/dev/null
e2e_network
e2e_make_cert app.test
HAPROXY_NAME="conduit-e2e-haproxy-$E2E_ID"

# An upstream that reads a PROXY protocol header if the connection starts with one and says what it found
# ("v1 IP", "v2 IP", "none -"), then echoes.
docker run -d --name "conduit-e2e-ppup-$E2E_ID" --network "$NET" --ip 192.0.2.31 --network-alias echo.test \
  conduit-e2e-client python3 -c '
import socket, socketserver
def exact(s, n):
    b = b""
    while len(b) < n:
        c = s.recv(n - len(b))
        if not c:
            raise EOFError
        b += c
    return b
def header(s):
    p = s.recv(1, socket.MSG_PEEK)
    if p == b"P":
        line = b""
        while not line.endswith(b"\r\n"):
            line += exact(s, 1)
        return "v1", line.split()[2].decode()
    if p == b"\r":
        h = exact(s, 16)
        body = exact(s, int.from_bytes(h[14:16], "big"))
        if h[12] & 0xF == 0:
            return "v2", "local"
        fam = h[13] >> 4
        if fam == 1:
            return "v2", socket.inet_ntop(socket.AF_INET, body[:4])
        return "v2", socket.inet_ntop(socket.AF_INET6, body[:16])
    return "none", "-"
class H(socketserver.BaseRequestHandler):
    def handle(self):
        s = self.request
        s.settimeout(30)
        try:
            ver, ip = header(s)
            s.sendall((ver + " " + ip + "\n").encode())
            while True:
                d = s.recv(65536)
                if not d:
                    break
                s.sendall(d)
        except (OSError, EOFError):
            pass
class S(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
S(("0.0.0.0", 9100), H).serve_forever()
' >/dev/null
E2E_WHO="${E2E_WHO:-} conduit-e2e-ppup-$E2E_ID $HAPROXY_NAME conduit-e2e-pphold-$E2E_ID"

cat > "$WORK/conduit.yaml" <<'YAML'
sites:
  - port: 8443
    host: app.test
    tls:
      cert: /conf/cert.pem
      key: /conf/key.pem
    proxyProtocol:
      trustedSources: ["192.0.2.50/32"]
    ipFilter:
      allow: ["192.0.2.60/32"]
    proxy: "http://upstream.test:8080"
  - port: 9300
    proxyProtocol:
      trustedSources: ["192.0.2.50/32"]
    tcp:
      targets: ["echo.test:9100"]
      sendProxyProtocol: v2
  - port: 9301
    proxyProtocol:
      trustedSources: ["192.0.2.50/32"]
    tcp:
      targets: ["echo.test:9100"]
  - port: 9302
    proxyProtocol:
      trustedSources: ["192.0.2.50/32"]
    tcp:
      targets: ["echo.test:9100"]
      maxConnectionsPerIp: 1
YAML
docker run -d --name "$APP" --network "$NET" --ip 192.0.2.20 --network-alias app.test \
  -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" conduit -c /conf/conduit.yaml >/dev/null
for _ in $(seq 1 50); do
  if client sh -c 'nc -z -w 1 app.test 9301 && nc -z -w 1 app.test 8443'; then up=1; break; fi
  sleep 0.5
done
[ -n "${up:-}" ] || fail "conduit does not accept connections on 9301 and 8443"

cat > "$WORK/haproxy.cfg" <<'CFG'
global
    maxconn 200
defaults
    mode tcp
    timeout connect 3s
    timeout client 40s
    timeout server 40s
frontend https_v2
    bind *:443
    default_backend app_https_v2
backend app_https_v2
    server app 192.0.2.20:8443 send-proxy-v2
frontend https_v1
    bind *:444
    default_backend app_https_v1
backend app_https_v1
    server app 192.0.2.20:8443 send-proxy
frontend tcp_send
    bind *:3306
    default_backend app_tcp_send
backend app_tcp_send
    server app 192.0.2.20:9300 send-proxy-v2
frontend tcp_plain
    bind *:3307
    default_backend app_tcp_plain
backend app_tcp_plain
    server app 192.0.2.20:9301 send-proxy-v2
frontend tcp_limit
    bind *:3308
    default_backend app_tcp_limit
backend app_tcp_limit
    server app 192.0.2.20:9302 send-proxy-v2
CFG
docker run -d --name "$HAPROXY_NAME" --network "$NET" --ip 192.0.2.50 --network-alias lb.test \
  -v "$WORK/haproxy.cfg:/usr/local/etc/haproxy/haproxy.cfg:ro" "$HAPROXY" >/dev/null
for _ in $(seq 1 50); do
  if client sh -c 'nc -z -w 1 lb.test 443 && nc -z -w 1 lb.test 3308'; then lb_up=1; break; fi
  sleep 0.5
done
[ -n "${lb_up:-}" ] || { docker logs "$HAPROXY_NAME" 2>&1 | tail -20; fail "HAProxy does not accept connections"; }
e2e_prove_isolation

# a client with a fixed address
from() { # from LAST_OCTET COMMAND...
  local ip="192.0.2.$1"
  shift
  docker run --rm --network "$NET" --ip "$ip" -v "$WORK:/conf:ro" conduit-e2e-client "$@"
}

# ── 1. HTTPS: the address in the header is the client's ──
via_lb() { # via_lb OCTET PORT: GET /headers through HAProxy; the body and, last, the status
  from "$1" curl -sS --max-time 10 --cacert /conf/cert.pem --resolve "app.test:$2:192.0.2.50" -H 'Host: app.test' \
    -w '\n%{http_code}' "https://app.test:$2/headers" 2>/dev/null || true
}
for pair in "443 v2" "444 v1"; do
  set -- $pair
  out=$(via_lb 60 "$1")
  [ "$(printf '%s\n' "$out" | tail -1)" = 200 ] || fail "HTTPS through HAProxy ($2 header) from 192.0.2.60: $out"
  printf '%s\n' "$out" | sed '$d' | jqe '.headers["X-Forwarded-For"] == ["192.0.2.60"]' >/dev/null \
    || fail "the upstream was not told the client of the $2 header (192.0.2.60): $out"
  ok "HTTPS through HAProxy with a $2 header: 200, and X-Forwarded-For is the client 192.0.2.60, not the load balancer"
done
out=$(via_lb 61 443)
[ "$(printf '%s\n' "$out" | tail -1)" = 403 ] \
  || fail "ipFilter did not see the client of the header: 192.0.2.61 is not on the allowlist but got: $out"
ok "ipFilter works on the client of the header: 192.0.2.61 gets 403 through the same load balancer that let .60 in"

# a client that skips the load balancer is not a trusted source: no TLS, whatever it claims
direct=$(from 60 curl -sS --max-time 10 --cacert /conf/cert.pem -o /dev/null -w '%{http_code}' https://app.test:8443/headers 2>&1 || true)
case "$direct" in
  *200*) fail "a client that bypassed the load balancer was served: $direct" ;;
esac
ok "a connection from an address that is not a trusted source gets no TLS ($direct)"
# the same, spoofing a header by hand
spoof=$(from 60 sh -c "printf 'PROXY TCP4 192.0.2.60 192.0.2.20 1 8443\r\n' | nc -w 3 app.test 8443 | wc -c" | tr -d '\r')
[ "$spoof" = 0 ] || fail "a spoofed header from an untrusted address was answered ($spoof bytes)"
ok "a PROXY header from an untrusted address is not believed (and not answered)"

# ── 2. tcp: accept, send, and consume ──
greet() { # greet OCTET PORT: the first line the upstream sends through HAProxy
  from "$1" sh -c "nc -w 2 lb.test $2 </dev/null | head -1" | tr -d '\r'
}
got=$(greet 60 3306)
[ "$got" = "v2 192.0.2.60" ] || fail "tcp 9300 (accept, send v2): the upstream reported '$got', expected 'v2 192.0.2.60'"
ok "tcp site: the upstream got a v2 header naming the client 192.0.2.60 ($got)"
got=$(greet 61 3306)
[ "$got" = "v2 192.0.2.61" ] || fail "tcp 9300 from .61: '$got'"
ok "tcp site: another client is named as itself ($got)"
got=$(greet 60 3307)
[ "$got" = "none -" ] || fail "tcp 9301 (accept, no send): the upstream reported '$got', expected 'none -'"
ok "tcp site without sendProxyProtocol: the header is consumed, the upstream sees a plain connection ($got)"
got=$(from 60 sh -c "nc -w 2 app.test 9300 </dev/null | head -1" | tr -d '\r')
[ -z "$got" ] || fail "a client that skipped the load balancer was relayed to the upstream: '$got'"
ok "tcp site: a client that skipped the load balancer is closed, nothing reaches the upstream"

# ── 3. tcp: the per-client cap counts the client of the header ──
probe='
import socket, sys, time
host, port, count, hold = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4])
socks, out = [], []
for _ in range(count):
    s = socket.create_connection((host, port), timeout=3)
    s.settimeout(1.5)
    try:
        out.append("ok" if s.recv(32).startswith(b"v2 ") else "refused")
    except OSError:
        out.append("refused")
    socks.append(s)
print(" ".join(out), flush=True)
time.sleep(hold)
'
docker run -d --name "conduit-e2e-pphold-$E2E_ID" --network "$NET" --ip 192.0.2.60 conduit-e2e-client \
  python3 -c "$probe" lb.test 3308 2 25 >/dev/null
for _ in $(seq 1 20); do
  first=$(docker logs "conduit-e2e-pphold-$E2E_ID" 2>&1 | tr -d '\r' | head -1)
  [ -n "$first" ] && break
  sleep 0.5
done
[ "$first" = "ok refused" ] || fail "maxConnectionsPerIp 1 from 192.0.2.60 through one load balancer: expected 'ok refused', got '$first'"
other=$(from 61 python3 -c "$probe" lb.test 3308 1 0 | tr -d '\r')
[ "$other" = "ok" ] || fail "another client was refused because of the first one's connection: '$other'"
ok "maxConnectionsPerIp counts the client of the header: 192.0.2.60 gets one connection ($first), 192.0.2.61 is not affected ($other)"

echo "proxy-protocol scenario passed"
