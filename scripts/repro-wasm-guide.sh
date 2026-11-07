#!/usr/bin/env bash
# Runs the plugins of docs/wasm.md against a real Conduit and checks what the guide says they do (issue #565).
#
#   python3 scripts/check_doc_wasm.py --conduit target/debug/conduit --keep /tmp/wasm-guide   # builds the plugins
#   scripts/repro-wasm-guide.sh [path/to/conduit] [dir with the built plugins]  (defaults: target/debug/conduit, /tmp/wasm-guide)
#
# Needs bash, curl and python3. Uses ports 4720 (upstream) and 8720 (Conduit) and checks that both are free. It starts
# and stops its own processes and removes its scratch directory. Exit status 0 when every expectation held.
set -u
CONDUIT=${1:-target/debug/conduit}
PLUGINS=${2:-/tmp/wasm-guide}
[ -x "$CONDUIT" ] || { echo "no executable at $CONDUIT (build one: cargo build --features full)" >&2; exit 2; }
[ -f "$PLUGINS/first_plugin.wasm" ] || { echo "no built plugins in $PLUGINS (run scripts/check_doc_wasm.py --keep $PLUGINS)" >&2; exit 2; }
WORK=$(mktemp -d)
PIDS=""
trap 'kill $PIDS 2>/dev/null; rm -rf "$WORK"' EXIT

for port in 4720 8720; do
  python3 -c 'import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))' "$port" 2>/dev/null || { echo "port $port is in use: stop what holds it and run again" >&2; exit 2; }
done

# An upstream that answers 200, or 500 on /fail.
cat > "$WORK/upstream.py" <<'PY'
import http.server
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        code = 500 if self.path == "/fail" else 200
        body = b"upstream\n"
        self.send_response(code)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a):
        pass
http.server.ThreadingHTTPServer(("127.0.0.1", 4720), H).serve_forever()
PY
python3 "$WORK/upstream.py" &
PIDS="$PIDS $!"

FAILED=0
expect() { # expect LABEL ACTUAL WANTED
  if [ "$2" = "$3" ]; then echo "ok    $1: $2"; else echo "FAIL  $1: got '$2', wanted '$3'"; FAILED=1; fi
}

# serve PLUGIN [CONFIG_YAML_LINES]: start Conduit with the plugin in front of the upstream
serve() {
  {
    printf 'sites:\n  - port: 8720\n    proxy: http://127.0.0.1:4720\n    middleware:\n      - type: wasm\n        path: %s\n' "$PLUGINS/$1.wasm"
    [ -n "${2:-}" ] && printf '%s\n' "$2"
  } > "$WORK/conduit.yaml"
  if [ -f "$PLUGINS/$1.wasm" ]; then
    "$CONDUIT" validate -c "$WORK/conduit.yaml" > "$WORK/validate.out" 2>&1 || { echo "FAIL  validate $1: $(cat "$WORK/validate.out")"; FAILED=1; }
  fi
  "$CONDUIT" -c "$WORK/conduit.yaml" > "$WORK/serve.log" 2>&1 &
  SERVER=$!
  PIDS="$PIDS $SERVER"
  ready=0
  for _ in $(seq 1 40); do
    if curl -s -o /dev/null -m 1 -H 'X-Api-Key: probe' http://127.0.0.1:8720/; then ready=1; break; fi
    sleep 0.25
  done
  if [ "$ready" = 0 ]; then
    echo "FAIL  Conduit did not answer for $1; its log:" >&2
    cat "$WORK/serve.log" >&2
    exit 1
  fi
}
stop() { kill "$SERVER" 2>/dev/null; wait "$SERVER" 2>/dev/null; }
status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
plain_log() { sed 's/\x1b\[[0-9;]*m//g' "$WORK/serve.log"; }

echo "== first plugin"
serve first_plugin
expect "no key" "$(status http://127.0.0.1:8720/)" 401
expect "no key, body" "$(curl -s http://127.0.0.1:8720/)" "missing X-Api-Key"
expect "with key" "$(status -H 'X-Api-Key: x' http://127.0.0.1:8720/)" 200
expect "the log line" "$(plain_log | grep -c 'missing X-Api-Key wasm=true')" 2
stop

echo "== key from config"
serve key_from_config '        config:
          allowed_key: "secret-abc"'
expect "right key" "$(status -H 'X-Api-Key: secret-abc' http://127.0.0.1:8720/)" 200
expect "wrong key" "$(status -H 'X-Api-Key: other' http://127.0.0.1:8720/)" 403
expect "no key" "$(status http://127.0.0.1:8720/)" 403
stop
serve key_from_config
expect "no config" "$(status -H 'X-Api-Key: secret-abc' http://127.0.0.1:8720/)" 503
stop

echo "== one module, both phases"
serve gate_and_tag
expect "no key" "$(status http://127.0.0.1:8720/)" 401
expect "ok response has no tag" "$(curl -s -D - -o /dev/null -H 'X-Api-Key: x' http://127.0.0.1:8720/ | grep -ci 'x-error-seen')" 0
expect "5xx response is tagged" "$(curl -s -D - -o /dev/null -H 'X-Api-Key: x' http://127.0.0.1:8720/fail | tr -d '\r' | grep -i 'x-error-seen')" "x-error-seen: 1"
expect "no link warnings" "$(plain_log | grep -ci 'unknown import')" 0
stop

echo "== a plugin that traps fails open"
serve always_traps
expect "the request passes" "$(status http://127.0.0.1:8720/)" 200
expect "the trap is logged" "$(plain_log | grep -c 'request passes through (fail-open)' | grep -qv '^0$' && echo yes || echo no)" yes
stop

echo "== a missing file"
serve absent
expect "the request passes" "$(status http://127.0.0.1:8720/)" 200
expect "validate says so" "$("$CONDUIT" validate -c "$WORK/conduit.yaml" 2>&1 | grep -c 'cannot be read')" 1
stop

[ "$FAILED" = 0 ] && echo "all expectations held" || echo "SOME EXPECTATIONS FAILED"
exit "$FAILED"
