#!/usr/bin/env bash
# Scenario: certificates and authorization with real clients (issue #704 follow-up).
#
#   scripts/e2e/auth-tls.sh BINARY      (or through run.sh)
#
# A private CA signs the server certificate and the client certificates. curl verifies the server certificate (and fails
# for the wrong name, an unknown CA, no CA); mutual TLS refuses a client with no certificate or one from another CA and
# serves one from the CA; then Basic auth, API key, consumers and JWT (HS256) each answer 401 without or with a wrong
# credential and 200 with the right one, honour `skipPaths`, and never let a client choose the identity the upstream sees.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client

# ── Certificates: CA, server (app.test), client (alice) and a client from a rogue CA ──
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

restart() { # restart CONFIG URL [curl args]: a fresh conduit on CONFIG
  docker rm -f "$APP" >/dev/null 2>&1 || true
  e2e_start_conduit "$@"
}
code() { client curl -sS --max-time 10 -o /dev/null -w '%{http_code}' "$@" 2>/dev/null || true; }
HDR='-H X-Probe:1'

TLS_BLOCK='tls:
  cert: /conf/app.test.pem
  key: /conf/app.test.key'

# ── 1. Server certificate ──
cat > "$WORK/tls.yaml" <<YAML
port: 8443
host: app.test
$TLS_BLOCK
proxy: "http://upstream.test:8080"
YAML
cp "$WORK/e2e-ca.pem" "$WORK/cert.pem"   # e2e_start_conduit verifies against /conf/cert.pem
restart "$WORK/tls.yaml" https://app.test:8443/get
[ "$(code --cacert /conf/e2e-ca.pem https://app.test:8443/get)" = 200 ] || fail "verified TLS request failed"
ok "a certificate from the CA verifies and is served"
code=0; client curl -sS --max-time 10 -o /dev/null https://app.test:8443/get 2>/dev/null || code=$?
[ "$code" = 60 ] || fail "curl without the CA exited $code, expected 60 (certificate not trusted)"
code=0; client curl -sS --max-time 10 --cacert /conf/rogue-ca.pem -o /dev/null https://app.test:8443/get 2>/dev/null || code=$?
[ "$code" = 60 ] || fail "curl with the wrong CA exited $code, expected 60"
code=0; client curl -sS --max-time 10 --cacert /conf/e2e-ca.pem --connect-to other.test:8443:app.test:8443 -o /dev/null https://other.test:8443/get 2>/dev/null || code=$?
[ "$code" = 60 ] || fail "curl for a name the certificate does not cover exited $code, expected 60"
ok "an unknown CA, the wrong CA and the wrong name are all refused by the client"

# ── 2. Mutual TLS ──
cat > "$WORK/mtls.yaml" <<YAML
port: 8443
host: app.test
$TLS_BLOCK
  clientAuth:
    ca: /conf/e2e-ca.pem
    optional: false
proxy: "http://upstream.test:8080"
YAML
restart "$WORK/mtls.yaml" https://app.test:8443/get --cert /conf/alice.pem --key /conf/alice.key
[ "$(code --cacert /conf/e2e-ca.pem --cert /conf/alice.pem --key /conf/alice.key https://app.test:8443/get)" = 200 ] \
  || fail "a client certificate from the CA was refused"
ok "mTLS: a client certificate from the CA is served"
code=0; client curl -sS --max-time 10 --cacert /conf/e2e-ca.pem -o /dev/null https://app.test:8443/get 2>/dev/null || code=$?
[ "$code" != 0 ] || fail "mTLS served a client with no certificate"
code=0; client curl -sS --max-time 10 --cacert /conf/e2e-ca.pem --cert /conf/mallory.pem --key /conf/mallory.key \
  -o /dev/null https://app.test:8443/get 2>/dev/null || code=$?
[ "$code" != 0 ] || fail "mTLS served a client certificate from another CA"
ok "mTLS: no certificate and a certificate from another CA are refused at the handshake"

sed -i 's/optional: false/optional: true/' "$WORK/mtls.yaml"
restart "$WORK/mtls.yaml" https://app.test:8443/get
[ "$(code --cacert /conf/e2e-ca.pem https://app.test:8443/get)" = 200 ] || fail "optional mTLS refused a client with no certificate"
code=0; client curl -sS --max-time 10 --cacert /conf/e2e-ca.pem --cert /conf/mallory.pem --key /conf/mallory.key \
  -o /dev/null https://app.test:8443/get 2>/dev/null || code=$?
[ "$code" != 0 ] || fail "optional mTLS accepted a certificate from another CA"
ok "mTLS optional: no certificate is served, a certificate from another CA is still refused"

# ── 3. Basic auth, API key ──
PLAIN='port: 8080
host: app.test
proxy: "http://upstream.test:8080"'
cat > "$WORK/basic.yaml" <<YAML
$PLAIN
basicAuth:
  users:
    alice: "s3cret-pw"
  realm: "e2e"
  skipPaths: [/anything/public/**]
YAML
restart "$WORK/basic.yaml" http://app.test:8080/get -u alice:s3cret-pw
U=http://app.test:8080
[ "$(code $U/get)" = 401 ] || fail "basic: no credentials served"
[ "$(code -u alice:wrong $U/get)" = 401 ] || fail "basic: wrong password served"
[ "$(code -u mallory:s3cret-pw $U/get)" = 401 ] || fail "basic: wrong user served"
[ "$(code -u alice:s3cret-pw $U/get)" = 200 ] || fail "basic: right credentials refused"
[ "$(code $U/anything/public/x)" = 200 ] || fail "basic: skipPaths not honoured"
client curl -sSI --max-time 10 $U/get | grep -qi '^www-authenticate: Basic realm="e2e"' || fail "basic: no WWW-Authenticate challenge"
ok "basic auth: 401 without, wrong user and wrong password; 200 with; skipPaths bypass; realm is challenged"

cat > "$WORK/apikey.yaml" <<YAML
$PLAIN
apiKey:
  keys: ["key-one", "key-two"]
  header: X-API-Key
YAML
restart "$WORK/apikey.yaml" http://app.test:8080/get -H 'X-API-Key: key-one'
[ "$(code $U/get)" = 401 ] || fail "apiKey: no key served"
[ "$(code -H 'X-API-Key: nope' $U/get)" = 401 ] || fail "apiKey: wrong key served"
[ "$(code -H 'X-API-Key: key-two' $U/get)" = 200 ] || fail "apiKey: second key refused"
[ "$(code -H 'X-API-Key;' $U/get)" = 401 ] || fail "apiKey: empty key served"
ok "api key: 401 without, wrong and empty; 200 with either configured key"

# ── 4. Consumers: identity is set by Conduit, never by the client ──
cat > "$WORK/consumers.yaml" <<YAML
$PLAIN
consumers:
  idHeader: X-Consumer-ID
  consumers:
    - username: alice
      apiKey: "alice-key"
      headers: { X-Tier: free }
    - username: bob
      apiKey: "bob-key"
YAML
restart "$WORK/consumers.yaml" http://app.test:8080/get -H 'X-API-Key: alice-key'
[ "$(code $U/get)" = 401 ] || fail "consumers: no key served"
out=$(client curl -sS --max-time 10 -H 'X-API-Key: alice-key' -H 'X-Consumer-ID: bob' -H 'X-Tier: premium' $U/headers)
echo "$out" | jqe '.headers["X-Consumer-Id"] == ["alice"] and .headers["X-Tier"] == ["free"]' >/dev/null \
  || fail "the client chose its identity or tier: $out"
out=$(client curl -sS --max-time 10 -H 'X-API-Key: bob-key' $U/headers)
echo "$out" | jqe '.headers["X-Consumer-Id"] == ["bob"]' >/dev/null || fail "bob was not identified as bob: $out"
ok "consumers: the upstream sees the identity and headers Conduit set, not the client's X-Consumer-ID / X-Tier"

# ── 5. JWT (HS256) ──
SECRET='e2e-jwt-secret-of-reasonable-length-0123456789'
mkjwt() { # mkjwt SECRET ALG EXP-OFFSET SUB  -> token (python, in the client image)
  docker run --rm conduit-e2e-client python3 -c '
import base64, hashlib, hmac, json, sys, time
secret, alg, off, sub = sys.argv[1:5]
b = lambda d: base64.urlsafe_b64encode(d).rstrip(b"=").decode()
head = b(json.dumps({"alg": alg, "typ": "JWT"}).encode())
body = b(json.dumps({"sub": sub, "role": "admin", "exp": int(time.time()) + int(off)}).encode())
sig = "" if alg == "none" else b(hmac.new(secret.encode(), (head + "." + body).encode(), hashlib.sha256).digest())
print(head + "." + body + "." + sig)' "$@"
}
cat > "$WORK/jwt.yaml" <<YAML
$PLAIN
jwtAuth:
  secret: "$SECRET"
  skipPaths: [/anything/public/**]
requestTransform:
  setHeaders:
    X-User: "{{ jwt.sub }}"
YAML
good=$(mkjwt "$SECRET" HS256 3600 carol)
restart "$WORK/jwt.yaml" http://app.test:8080/get -H "Authorization: Bearer $good"
bearer() { printf 'Authorization: Bearer %s' "$1"; }
[ "$(code $U/get)" = 401 ] || fail "jwt: no token served"
[ "$(code -H "$(bearer "$good")" $U/get)" = 200 ] || fail "jwt: valid token refused"
[ "$(code -H "$(bearer "$(mkjwt "$SECRET" HS256 -3600 carol)")" $U/get)" = 401 ] || fail "jwt: expired token served (leeway is 60 s)"
[ "$(code -H "$(bearer "$(mkjwt wrong-secret-wrong-secret-wrong-secret HS256 3600 carol)")" $U/get)" = 401 ] || fail "jwt: token with another secret served"
[ "$(code -H "$(bearer "$(mkjwt "$SECRET" none 3600 carol)")" $U/get)" = 401 ] || fail "jwt: alg=none served"
tampered="${good%%.*}.$(echo -n '{"sub":"root","role":"admin","exp":9999999999}' | base64 -w0 | tr '+/' '-_' | tr -d =).${good##*.}"
[ "$(code -H "$(bearer "$tampered")" $U/get)" = 401 ] || fail "jwt: tampered payload served"
[ "$(code -H "$(bearer "")" $U/get)" = 401 ] || fail "jwt: empty bearer served"
[ "$(code $U/anything/public/x)" = 200 ] || fail "jwt: skipPaths not honoured"
out=$(client curl -sS --max-time 10 -H "$(bearer "$good")" -H 'X-User: root' $U/headers)
echo "$out" | jqe '.headers["X-User"] == ["carol"]' >/dev/null || fail "the upstream saw a user the token does not name: $out"
ok "jwt: 401 without, expired, wrong secret, alg=none, tampered, empty; 200 valid; claims reach the upstream and a client's X-User does not"

echo "auth-tls scenario passed"
