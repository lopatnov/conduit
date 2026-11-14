#!/usr/bin/env bash
# Scenario: the TCP proxy (`sites[].tcp`) with real clients (issue #704 follow-up).
#
#   scripts/e2e/tcp.sh BINARY      (or through run.sh)
#
# Two line-echo servers (tcp1, tcp2: each greets with its own name, then echoes) behind a Conduit TCP site. Checks that
# connections alternate under round-robin, that bytes go both ways intact (1 MiB, compared by SHA-256), that a client
# closing its write side still gets the reply, that an upstream that is down is skipped or reported without hanging the
# client, that a target that never answers hits `connectTimeoutMs`, and that a second site does not share targets.
# Needs `--features tcp` in the binary.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
e2e_network

i=0
for name in tcp1 tcp2; do
  i=$((i + 1))
  docker run -d --name "conduit-e2e-tcp-$name-$E2E_ID" --network "$NET" --ip "192.0.2.$((30 + i))" --network-alias "$name.test" \
    conduit-e2e-client python3 -c '
import socketserver, sys
name = sys.argv[1]
class H(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.sendall((name + "\n").encode())
        while True:
            d = self.request.recv(65536)
            if not d:
                break
            self.request.sendall(d)
class S(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
S(("0.0.0.0", 9100), H).serve_forever()
' "$name" >/dev/null
  E2E_WHO="${E2E_WHO:-} conduit-e2e-tcp-$name-$E2E_ID"
done
sleep 1

cat > "$WORK/scenario.yaml" <<'YAML'
sites:
  - port: 9000
    tcp:
      targets: ["tcp1.test:9100", "tcp2.test:9100"]
      strategy: round-robin
  - port: 9001
    tcp:
      targets: ["tcp2.test:9100"]
  - port: 9002
    tcp:
      targets: ["192.0.2.99:9100"]
      connectTimeoutMs: 1000
  - port: 9003
    tcp:
      targets: ["tcp1.test:9100", "tcp2.test:9100"]
      strategy: random
YAML
docker rm -f "$APP" >/dev/null 2>&1 || true
cp "$WORK/scenario.yaml" "$WORK/conduit.yaml"
docker run -d --name "$APP" --network "$NET" --ip 192.0.2.20 --network-alias app.test \
  -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" conduit -c /conf/conduit.yaml >/dev/null
for _ in $(seq 1 50); do
  if client sh -c 'nc -z -w 1 app.test 9000'; then up=1; break; fi
  sleep 0.5
done
[ -n "${up:-}" ] || fail "the TCP proxy does not accept connections on 9000"
e2e_prove_isolation

# greet PORT: connect, read the greeting line (the name of the upstream), send "ping", read the echo.
greet() { client sh -c 'printf "ping\n" | nc -w 2 app.test '"$1"' | tr "\n" " "'; }

# 1. round-robin: alternating
seq=$(for _ in 1 2 3 4 5 6; do greet 9000 | awk '{print $1}'; done | tr '\n' ' ')
[ "$seq" = "tcp1 tcp2 tcp1 tcp2 tcp1 tcp2 " ] || [ "$seq" = "tcp2 tcp1 tcp2 tcp1 tcp2 tcp1 " ] || fail "round-robin did not alternate: $seq"
[ "$(greet 9000 | awk '{print $2}')" = ping ] || fail "the echo did not come back through the proxy"
ok "round-robin alternates between the targets: $seq"

# 2. random: both targets get used
seen=$(for _ in $(seq 1 30); do greet 9003 | awk '{print $1}'; done | sort -u | wc -l)
[ "$seen" = 2 ] || fail "random used $seen of 2 targets in 30 connections"
ok "random reaches both targets"

# 3. 1 MiB each way, intact
sent=$(client sh -c 'head -c 1048576 /dev/urandom > /tmp/blob; sha256sum < /tmp/blob | cut -c1-64; (cat /tmp/blob; sleep 1) | nc -w 5 app.test 9001 | tail -c +6 | sha256sum | cut -c1-64' | tr '\n' ' ')
set -- $sent
[ "$1" = "$2" ] || fail "1 MiB through the proxy changed: sent $1, received $2"
ok "1 MiB goes through and comes back intact (sha256 $1)"

# 4. the client closes its write side, the reply still arrives (half-close)
out=$(client sh -c 'printf "last words\n" | nc -N -w 3 app.test 9001 | tr "\n" " "' || true)
echo "$out" | grep -q 'last words' || fail "after the client closed its write side the reply did not arrive: '$out'"
ok "half-close: the reply arrives after the client has shut down its write side"

# 5. a target that never answers: the connection is dropped around connectTimeoutMs, the client does not hang
# Timed inside the client container, in milliseconds, so the start-up of `docker run` is not part of it. An address that
# nobody owns on the bridge fails in about 3 s by itself (ARP), so a bound under that tells a connect timeout of 1 s
# from the kernel giving up.
ms=$(client sh -c 's=$(date +%s%3N); printf "x\n" | nc -w 8 app.test 9002 >/dev/null; echo $(( $(date +%s%3N) - s ))' || true)
[ -n "$ms" ] && [ "$ms" -le 2500 ] || fail "a blackhole target held the client for ${ms:-?} ms with connectTimeoutMs 1000"
ok "a target that never answers is cut off in ${ms} ms (connectTimeoutMs: 1000)"

# 6. one upstream goes down: round-robin keeps serving through the other one, or reports it, never hangs
docker stop "conduit-e2e-tcp-tcp1-$E2E_ID" >/dev/null
ok_n=0; for _ in $(seq 1 6); do greet 9000 | grep -q 'tcp2' && ok_n=$((ok_n + 1)); done
echo "with tcp1 down, $ok_n of 6 connections were served by tcp2"
[ "$ok_n" -ge 3 ] || fail "with one target down fewer than half the connections were served ($ok_n of 6)"
ok "one target down: $ok_n of 6 connections served by the live one, none hung"

echo "tcp scenario passed"
