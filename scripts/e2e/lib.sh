#!/usr/bin/env bash
# Shared helpers of the Docker end-to-end tests: Conduit under test, a real upstream and real clients in a Docker network
# that has no route to the Internet. Sourced by the scenarios in this directory (run them with run.sh).
#
# What is in the network (192.0.2.0/24 is TEST-NET-1, RFC 5737; `.test` names are reserved for tests, RFC 6761):
#   upstream.test   192.0.2.10   go-httpbin (pinned by digest): echoes the headers and bodies it received
#   app.test        192.0.2.20   the conduit under test, started by a scenario with its own configuration
#   (the clients)                curl, nghttp and jq from a throw-away container on the same network
# The network is created with `--internal`, and `e2e_prove_isolation` shows it: a client cannot reach the Internet.
#
# These tests make no claim about one feature: a scenario is any behaviour of the application that needs a real client or
# a real upstream rather than the in-process fixtures of tests/ (HTTP/2 with another implementation, upstream protocol
# behaviour, TLS with a verifying client, ...). Add a scenario as a new file here; run.sh picks it up.
#
# Needs Docker and openssl on the host. Environment: CONDUIT_IMAGE, the image the binary runs in (default ubuntu:24.04 by digest: the
# binary of the CI job is built on its glibc).

E2E_UBUNTU=ubuntu@sha256:2198e4c65076e9cb0b3d695219df667c0e3b0072da64ca3c2648aca17dcecaf5
E2E_DEBIAN=debian@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
E2E_HTTPBIN=mccutchen/go-httpbin@sha256:3992f3763e9ce5a4307eae0a869a78b4df3931dc8feba74ab823dd2444af6a6b

# e2e_init BINARY: names, temporary directory and the cleanup that runs when the scenario ends, however it ends.
e2e_init() {
  BIN=$(realpath "$1")
  CONDUIT_IMAGE="${CONDUIT_IMAGE:-$E2E_UBUNTU}"
  E2E_ID=$$
  NET="conduit-e2e-$E2E_ID"
  APP="conduit-e2e-app-$E2E_ID"
  UP="conduit-e2e-upstream-$E2E_ID"
  WORK=$(mktemp -d)
  trap e2e_cleanup EXIT
}

e2e_cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "--- conduit log"
    docker logs "$APP" 2>&1 | tail -30 || true
  fi
  docker rm -f "$APP" "$UP" ${E2E_WHO:-} >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

ok() {
  echo "ok: $*"
}

# e2e_build_client: the client image. It needs the Internet, so it is built before the network exists.
e2e_build_client() {
  # E2E_SKIP_CLIENT_BUILD=1: the image `conduit-e2e-client` was built by hand (a sandbox whose build containers cannot
  # reach the Debian mirror); it needs curl, nghttp, jq, openssl and nc.
  if [ -n "${E2E_SKIP_CLIENT_BUILD:-}" ]; then
    docker image inspect conduit-e2e-client >/dev/null || fail "E2E_SKIP_CLIENT_BUILD is set but there is no conduit-e2e-client image"
    docker pull -q "$E2E_HTTPBIN" >/dev/null
    return
  fi
  docker build -q -t conduit-e2e-client - >/dev/null <<EOF
FROM $E2E_DEBIAN
RUN apt-get update -qq \\
 && apt-get install -y -qq --no-install-recommends curl ca-certificates nghttp2-client jq openssl netcat-openbsd python3 >/dev/null \\
 && rm -rf /var/lib/apt/lists/*
EOF
  docker pull -q "$E2E_HTTPBIN" >/dev/null
}

# e2e_make_cert NAME...: a certificate for the names into $WORK (cert.pem, key.pem); the clients verify it.
e2e_make_cert() {
  local san="" name
  for name in "$@"; do san="$san,DNS:$name"; done
  openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj "/CN=$1" \
    -addext "subjectAltName=${san#,}" \
    -keyout "$WORK/key.pem" -out "$WORK/cert.pem" 2>/dev/null
  chmod 644 "$WORK/key.pem"
}

# e2e_network: the isolated network and the upstream in it.
e2e_network() {
  docker network create --internal --subnet 192.0.2.0/24 "$NET" >/dev/null
  docker run -d --name "$UP" --network "$NET" --ip 192.0.2.10 --network-alias upstream.test "$E2E_HTTPBIN" >/dev/null
}

# e2e_start_conduit CONFIG: copy CONFIG to $WORK/conduit.yaml and start the binary on it; /conf in the container is $WORK
# (the certificate is /conf/cert.pem and /conf/key.pem). Returns when conduit answers on URL (the second argument); more
# arguments are given to the curl that waits (a client certificate, for instance).
e2e_start_conduit() {
  cp "$1" "$WORK/conduit.yaml"
  docker run -d --name "$APP" --network "$NET" --ip 192.0.2.20 --network-alias app.test \
    -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" \
    conduit -c /conf/conduit.yaml >/dev/null
  local up=""
  for _ in $(seq 1 50); do
    if client curl -sS --max-time 10 --cacert /conf/cert.pem "${@:3}" -o /dev/null "$2" 2>/dev/null; then up=1; break; fi
    sleep 0.5
  done
  [ -n "$up" ] || fail "conduit does not answer on $2"
}

# client COMMAND...: a throw-away client container on the network, with $WORK mounted at /conf.
client() { docker run --rm --network "$NET" -v "$WORK:/conf:ro" conduit-e2e-client "$@"; }

# jqe FILTER: jq -e on stdin, in the client image (the host may not have jq); it needs no network, so it has none.
jqe() { docker run --rm -i --network none conduit-e2e-client jq -e "$@"; }

# e2e_prove_isolation: a client cannot reach the Internet.
e2e_prove_isolation() {
  if client curl -sS --max-time 4 -o /dev/null https://example.com 2>/dev/null; then
    fail "the test network has a route to the Internet"
  fi
  ok "isolated network"
}

# e2e_who_upstreams NAME...: one tiny upstream per name (who1.test is the first, 192.0.2.11, and so on); a GET to any path
# answers with the name, so a client can tell which of them the balancer chose. They run the client image (python3).
# HTTP on port 8080; the same server also answers `/slow` after two seconds (for least-conn), `/status/N` with N,
# `/headers` with the request headers as JSON (a header map: names as received) and `/seen` with every request line it
# has served so far, one per line (the oracle for smuggling and traversal checks: what did the upstream really get).
e2e_who_upstreams() {
  local i=0 name
  for name in "$@"; do
    i=$((i + 1))
    docker run -d --name "conduit-e2e-who-$name-$E2E_ID" --network "$NET" --ip "192.0.2.$((10 + i))" \
      --network-alias "$name.test" conduit-e2e-client python3 -c '
import http.server, json, sys, time
name = sys.argv[1]
SEEN = []
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "who"
    sys_version = ""
    def parse_request(self):
        # every request line that parses, whatever the method: a smuggled `GPOST /` never reaches do_GET (Python
        # answers 501 for it), and the oracle must still see it
        ok = super().parse_request()
        if ok and self.path != "/seen":
            SEEN.append(self.command + " " + self.path)
        return ok
    def do_GET(self):
        code = 200
        if self.path == "/seen":
            body = ("\n".join(SEEN) + "\n").encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path.startswith("/headers"):
            body = json.dumps({k: v for k, v in self.headers.items()}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path.startswith("/slow"):
            time.sleep(2)
        elif self.path.startswith("/status/"):
            code = int(self.path.rsplit("/", 1)[1])
        body = (name + "\n").encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
    do_POST = do_GET
    do_HEAD = do_GET
    def log_message(self, *a):
        pass
http.server.ThreadingHTTPServer(("0.0.0.0", 8080), H).serve_forever()
' "$name" >/dev/null
    E2E_WHO="${E2E_WHO:-} conduit-e2e-who-$name-$E2E_ID"
  done
  sleep 1
}
