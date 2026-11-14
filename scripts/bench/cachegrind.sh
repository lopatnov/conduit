#!/usr/bin/env bash
# Issue #486: instructions per proxied request, deterministic to well under 1 % — a cost model that needs no
# quiet machine. Runs conduit under `valgrind --tool=cachegrind`, drives N1 and then N2 keep-alive requests
# through it to a local upstream, and reports the SLOPE (Ir(N2) - Ir(N1)) / (N2 - N1): start-up cost cancels.
#   cachegrind.sh CONDUIT_BIN [N1] [N2]
# EXTRA_SITE='"securityHeaders": true, "responseTime": true' adds site fields (a response chain with
# extra headers / transforms costs more than the bare default site, which hides per-response clones).
# Needs: valgrind, python3. Linux only. A build with debug info (`CARGO_PROFILE_RELEASE_DEBUG=1`) lets
# `cg_annotate` name the functions; the slope does not need it.
set -uo pipefail
BIN=${1:?usage: cachegrind.sh BIN [N1] [N2]}
N1=${2:-2000}
N2=${3:-6000}
KEEP=${KEEP:-}   # set to a directory: the cachegrind.out files (cg.N1, cg.N2) are copied there for cg_annotate
WORK=$(mktemp -d)
trap 'kill -9 $(jobs -p) 2>/dev/null; if [ -n "$KEEP" ]; then mkdir -p "$KEEP" && cp "$WORK"/cg.* "$KEEP"/ && echo "cachegrind output kept in $KEEP" >&2; fi; rm -rf "$WORK"' EXIT

EXTRA_SITE=${EXTRA_SITE:-}
PORT=${PORT:-18080}; ADMIN=${ADMIN:-12019}; UP=${UP:-14000}

cat > "$WORK/driver.py" <<'PY'
import http.server, socket, sys, threading, time

up_port, port, n = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3])

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def do_GET(self):
        body = b"ok"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    do_HEAD = do_GET
    def log_message(self, *a):
        pass

srv = http.server.ThreadingHTTPServer(("127.0.0.1", up_port), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()

def wait(p):
    for _ in range(600):
        try:
            socket.create_connection(("127.0.0.1", p), 0.2).close()
            return
        except OSError:
            time.sleep(0.1)
    sys.exit("port %d never opened" % p)

wait(port)
req = b"GET /x HTTP/1.1\r\nHost: bench.test\r\nUser-Agent: cg\r\nAccept: */*\r\n\r\n"
s = socket.create_connection(("127.0.0.1", port))
buf = b""
for i in range(n):
    s.sendall(req)
    while b"\r\n\r\n" not in buf or not buf.endswith(b"ok"):
        chunk = s.recv(65536)
        if not chunk:
            sys.exit("connection closed after %d requests" % i)
        buf += chunk
    if not buf.startswith(b"HTTP/1.1 200"):
        sys.exit("bad status at request %d: %r" % (i, buf[:40]))
    buf = b""
s.close()
PY

run_one() {  # n -> prints total instructions
  local n=$1 out="$WORK/cg.$1"
  printf '{ "global": { "workers": 1, "shutdownTimeoutSecs": 1, "admin": { "bind": "127.0.0.1:%s" } }, "sites": [ { "port": %s, "proxy": "http://127.0.0.1:%s", "logging": false%s } ] }' \
    "$ADMIN" "$PORT" "$UP" "${EXTRA_SITE:+, $EXTRA_SITE}" > "$WORK/conduit.json"
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$out" "$BIN" -c "$WORK/conduit.json" \
    > "$WORK/valgrind.$n.log" 2>&1 &
  local vg=$!
  python3 "$WORK/driver.py" "$UP" "$PORT" "$n" || { echo "driver failed" >&2; kill -9 $vg; return 1; }
  curl -s -X POST "http://127.0.0.1:$ADMIN/shutdown" > /dev/null
  wait $vg 2>/dev/null
  awk '/^summary:/ { print $2 }' "$out"
}

IR1=$(run_one "$N1") || exit 1
IR2=$(run_one "$N2") || exit 1
python3 - "$IR1" "$IR2" "$N1" "$N2" <<'PY'
import sys
i1, i2, n1, n2 = map(int, sys.argv[1:])
print(f"Ir({n1}) = {i1}\nIr({n2}) = {i2}\ninstructions per request = {(i2 - i1) / (n2 - n1):.0f}")
PY
