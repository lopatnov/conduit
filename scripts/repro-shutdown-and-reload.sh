#!/usr/bin/env bash
# Reproduces the two claims of docs/configuration.md, "What happens on SIGTERM" (issues #36, #655):
#
#   1. a request that is still open when `global.shutdownTimeoutSecs` ends is cut, over HTTP/1.1 and over HTTP/2;
#   2. a hot reload that changes the upstream does not cut a request that is already in flight.
#
#   scripts/repro-shutdown-and-reload.sh [path/to/conduit]      (default: target/debug/conduit)
#
# Needs bash, curl, python3 and, for the HTTP/2 case, openssl (the case is skipped without it). Uses ports 4711, 4712,
# 8717, 8718, 8716 and the Admin API on 127.0.0.1:2019; run it on a machine where they are free. It starts and stops its
# own processes and removes its scratch directory.
set -u
CONDUIT=${1:-target/debug/conduit}
[ -x "$CONDUIT" ] || { echo "no executable at $CONDUIT (build one: cargo build)" >&2; exit 2; }
WORK=$(mktemp -d)
PIDS=""
trap 'kill $PIDS 2>/dev/null; rm -rf "$WORK"' EXIT

# Every port the script uses must be free: otherwise a health wait could succeed against someone else's server and a
# reload would be sent to it.
for port in 4711 4712 4713 8716 8717 8718 2019; do
  python3 -c 'import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))' "$port" 2>/dev/null || { echo "port $port is in use: stop what holds it and run again" >&2; exit 2; }
done

# The processes this script started, so that the trap stops them even on Ctrl-C.
track() { PIDS="$PIDS $1"; }
untrack() { PIDS=$(printf '%s
' $PIDS | grep -vx "$1" | tr '
' ' '); }

# A backend that answers after DELAY seconds: python3 slow.py NAME PORT DELAY
cat > "$WORK/slow.py" <<'PY'
import http.server, sys, time
name, port, delay = sys.argv[1], int(sys.argv[2]), float(sys.argv[3])
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        time.sleep(delay)
        body = (name + "\n").encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
PY

start_backend() { python3 "$WORK/slow.py" "$1" "$2" "$3" & PIDS="$PIDS $!"; }
now() { python3 -c 'import time; print(time.time())'; }

# 1. SIGTERM with a request open: LABEL SCHEME CURL_FLAGS PORT EXTRA_SITE_YAML
sigterm_case() {
  label=$1; scheme=$2; flags=$3; port=$4; extra=$5
  cat > "$WORK/sigterm.yaml" <<EOF
global:
  shutdownTimeoutSecs: 1
sites:
  - port: $port
$extra
    proxy: http://127.0.0.1:4711
EOF
  "$CONDUIT" -c "$WORK/sigterm.yaml" > "$WORK/sigterm.log" 2>&1 &
  conduit_pid=$!
  track "$conduit_pid"
  for _ in $(seq 1 40); do curl -sk $flags -o /dev/null -m 1 "$scheme://127.0.0.1:$port/__health__" && break; sleep 0.25; done
  ( curl -sk $flags -m 30 -w "client: status %{http_code} after %{time_total}s" -o /dev/null "$scheme://127.0.0.1:$port/" > "$WORK/client.out" 2>&1 ) &
  client_pid=$!
  sleep 1
  start=$(now)
  kill -TERM "$conduit_pid"
  wait "$conduit_pid" 2>/dev/null
  untrack "$conduit_pid"
  end=$(now)
  wait "$client_pid" 2>/dev/null
  awk -v a="$start" -v b="$end" -v l="$label" -v c="$(cat "$WORK/client.out")" \
    'BEGIN { printf "%s: the process exited %.1f s after SIGTERM (shutdownTimeoutSecs 1); %s\n", l, b - a, c }'
}

start_backend A-slow 4711 12
sleep 1
sigterm_case "HTTP/1.1" http "" 8717 ""
if command -v openssl >/dev/null 2>&1; then
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$WORK/key.pem" -out "$WORK/cert.pem" -days 1 -subj "/CN=localhost" >/dev/null 2>&1
  sigterm_case "HTTP/2 over TLS" https "--http2" 8718 "    tls: { cert: $WORK/cert.pem, key: $WORK/key.pem }
    http2: {}"
else
  echo "HTTP/2 over TLS: skipped (no openssl)"
fi
echo "(a client status of 000 after about 2 s means the connection was cut at the end of the grace period, 1 s after the signal)"

# 2. A reload that changes the upstream while a request is open
start_backend A-slow-4s 4712 4
sleep 1
cat > "$WORK/reload.yaml" <<EOF
global:
  admin: { bind: "127.0.0.1:2019" }
sites:
  - port: 8716
    proxy: http://127.0.0.1:4712
EOF
"$CONDUIT" -c "$WORK/reload.yaml" > "$WORK/reload.log" 2>&1 &
conduit_pid=$!
track "$conduit_pid"
for _ in $(seq 1 40); do curl -s -o /dev/null -m 1 http://127.0.0.1:2019/health && break; sleep 0.25; done
( curl -s -m 30 -w ' [client: status %{http_code} after %{time_total}s]' http://127.0.0.1:8716/ > "$WORK/open.out" 2>&1 ) &
open_pid=$!
sleep 1
start_backend B-fast 4713 0
sleep 1
sed -i.bak 's#4712#4713#' "$WORK/reload.yaml"
"$CONDUIT" reload -c "$WORK/reload.yaml" > /dev/null 2>&1
echo "after the reload, a new request: $(curl -s -m 5 http://127.0.0.1:8716/ | tr -d '\n')"
wait "$open_pid" 2>/dev/null
echo "the request that was open during the reload: $(tr -d '\n' < "$WORK/open.out")"
kill -KILL "$conduit_pid" 2>/dev/null
