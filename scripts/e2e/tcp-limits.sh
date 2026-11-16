#!/usr/bin/env bash
# Scenario: connection limits and the idle timeout of a TCP site (`tcp.maxConnections`, `tcp.maxConnectionsPerIp`,
# `tcp.idleTimeoutSecs`; issue #757), with real clients.
#
#   scripts/e2e/tcp-limits.sh BINARY      (or through run.sh)
#
# One line-echo server (greets with its name, then echoes) behind four Conduit TCP sites:
#   9200  maxConnections: 3          the fourth connection is closed at once, a freed slot is reused
#   9201  maxConnectionsPerIp: 2     a third connection from one address is refused, another address is not affected
#   9202  idleTimeoutSecs: 2         a quiet connection is closed after about 2 s, one with traffic is not
#   9203  idleTimeoutSecs: 0         no idle timeout: a quiet connection is still open after 5 s
# Needs `--features tcp` in the binary.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
e2e_network

docker run -d --name "conduit-e2e-tcplim-echo-$E2E_ID" --network "$NET" --ip 192.0.2.31 --network-alias echo.test \
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
E2E_WHO="${E2E_WHO:-} conduit-e2e-tcplim-echo-$E2E_ID"
sleep 1

cat > "$WORK/conduit.yaml" <<'YAML'
sites:
  - port: 9200
    tcp:
      targets: ["echo.test:9100"]
      maxConnections: 3
  - port: 9201
    tcp:
      targets: ["echo.test:9100"]
      maxConnectionsPerIp: 2
  - port: 9202
    tcp:
      targets: ["echo.test:9100"]
      idleTimeoutSecs: 2
  - port: 9203
    tcp:
      targets: ["echo.test:9100"]
      idleTimeoutSecs: 0
YAML
docker rm -f "$APP" >/dev/null 2>&1 || true
docker run -d --name "$APP" --network "$NET" --ip 192.0.2.20 --network-alias app.test \
  -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" conduit -c /conf/conduit.yaml >/dev/null
for _ in $(seq 1 50); do
  if client sh -c 'nc -z -w 1 app.test 9200'; then up=1; break; fi
  sleep 0.5
done
[ -n "${up:-}" ] || fail "the TCP proxy does not accept connections on 9200"
e2e_prove_isolation

# probe.py HOST PORT COUNT: open COUNT connections one after the other and keep them all open; print, per connection,
# "ok" when the upstream's greeting arrives within 1.5 s and "refused" when the proxy closes it (or nothing arrives).
probe='
import socket, sys, time
host, port, count = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
hold = float(sys.argv[4]) if len(sys.argv) > 4 else 0
socks, out = [], []
for _ in range(count):
    s = socket.create_connection((host, port), timeout=3)
    s.settimeout(1.5)
    try:
        out.append("ok" if s.recv(16).startswith(b"echo") else "refused")
    except OSError:
        out.append("refused")
    socks.append(s)
print(" ".join(out), flush=True)
time.sleep(hold)
'

# 1. maxConnections: 3 — the first three are served, the fourth and fifth are closed; a freed slot is used again
got=$(client python3 -c "$probe" app.test 9200 5 | tr -d '\r')
[ "$got" = "ok ok ok refused refused" ] || fail "maxConnections 3: expected 'ok ok ok refused refused', got '$got'"
ok "maxConnections: 3 serves three connections and closes the others at once ($got)"
sleep 1
again=$(client python3 -c "$probe" app.test 9200 4 | tr -d '\r')
[ "$again" = "ok ok ok refused" ] || fail "the slots of the closed client were not freed: '$again'"
ok "the slots are free again once the clients are gone ($again)"

# 2. maxConnectionsPerIp: 2 — one address gets two, another address is not affected while the first holds its two
docker rm -f "conduit-e2e-tcplim-holder-$E2E_ID" >/dev/null 2>&1 || true
docker run -d --name "conduit-e2e-tcplim-holder-$E2E_ID" --network "$NET" --ip 192.0.2.41 conduit-e2e-client \
  python3 -c "$probe" app.test 9201 4 25 >/dev/null
E2E_WHO="${E2E_WHO:-} conduit-e2e-tcplim-holder-$E2E_ID"
for _ in $(seq 1 20); do
  first=$(docker logs "conduit-e2e-tcplim-holder-$E2E_ID" 2>&1 | tr -d '\r' | head -1)
  [ -n "$first" ] && break
  sleep 0.5
done
[ "$first" = "ok ok refused refused" ] || fail "maxConnectionsPerIp 2 from one address: expected 'ok ok refused refused', got '$first'"
other=$(client python3 -c "$probe" app.test 9201 2 | tr -d '\r')
[ "$other" = "ok ok" ] || fail "another client address was affected by the first one's limit: '$other'"
ok "maxConnectionsPerIp: 2 refuses the third connection of one address ($first) and leaves another address alone ($other)"
docker rm -f "conduit-e2e-tcplim-holder-$E2E_ID" >/dev/null 2>&1 || true

# 3. idleTimeoutSecs: 2 — a quiet connection is closed after about two seconds; one that carries traffic stays open
idle=$(client python3 -c '
import socket, time
s = socket.create_connection(("app.test", 9202), timeout=10)
s.recv(16)
t = time.time()
try:
    closed = s.recv(16) == b""
except OSError:
    closed = True
print("closed" if closed else "open", round(time.time() - t, 1))
' | tr -d '\r')
set -- $idle
[ "$1" = closed ] || fail "a quiet connection was not closed (idleTimeoutSecs 2): $idle"
awk "BEGIN { exit !($2 >= 1.5 && $2 <= 4.5) }" || fail "closed after $2 s, expected about 2 s"
ok "idleTimeoutSecs: 2 closes a quiet connection after $2 s"
busy=$(client python3 -c '
import socket, time
s = socket.create_connection(("app.test", 9202), timeout=10)
s.recv(16)
start = time.time()
for _ in range(6):
    time.sleep(1)
    s.sendall(b"x")
    assert s.recv(1) == b"x"
print("open", round(time.time() - start, 1))
' | tr -d '\r')
# All six exchanges completed (a cut connection raises, so nothing is printed) and the connection lived well past
# the 2 s idle time; the elapsed time is only bounded from below, since a slow runner may take longer than 6 s.
set -- $busy
[ "$1" = open ] || fail "a connection with traffic every second was cut at idleTimeoutSecs 2: '$busy'"
awk "BEGIN { exit !($2 >= 5.5) }" || fail "traffic every second finished after $2 s, expected at least 5.5 s: '$busy'"
ok "a connection with traffic stays open past the idle time ($2 s)"

# 4. idleTimeoutSecs: 0 — no idle timeout
still=$(client python3 -c '
import socket
s = socket.create_connection(("app.test", 9203), timeout=10)
s.recv(16)
s.settimeout(5)
try:
    print("closed" if s.recv(16) == b"" else "data")
except socket.timeout:
    print("open")
' | tr -d '\r')
[ "$still" = open ] || fail "idleTimeoutSecs 0 closed a quiet connection: '$still'"
ok "idleTimeoutSecs: 0 leaves a quiet connection open"

echo "tcp-limits scenario passed"
