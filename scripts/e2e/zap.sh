#!/usr/bin/env bash
# Scenario: OWASP ZAP against Conduit (issue #705).
#
#   scripts/e2e/zap.sh BINARY      (or through run.sh)
#   E2E_SKIP_ZAP=1 scripts/e2e/run.sh BINARY      skips it (the ZAP image is 3.7 GB)
#   E2E_ZAP_ACTIVE=1 scripts/e2e/zap.sh BINARY     also runs the active scan (minutes, sends attack traffic)
#
# ZAP (pinned by digest, run offline with `-silent`) spiders Conduit, which sits in front of a plain text upstream and has
# every response header the configuration offers switched on, and passively checks every response. The baseline must
# finish with no warning and no failure: a missing security header, a leaking `Server` header, a cookie flag, a CORS
# wildcard or an error page that discloses information would show up here. The upstream is deliberately boring, so any alert is
# Conduit's. The attack classes of issue #705 that need a raw socket (smuggling, injection, traversal) are in guards.sh.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

E2E_ZAP=zaproxy/zap-stable@sha256:9624669db72f54d86c4b46a7064518bd9bf79d8c61a4c31befa9923489f328af

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
if [ -n "${E2E_SKIP_ZAP:-}" ]; then
  echo "skipped (E2E_SKIP_ZAP is set: the ZAP image is 3.7 GB)"
  exit 0
fi
e2e_init "$1"
e2e_build_client
docker pull -q "$E2E_ZAP" >/dev/null
e2e_network
e2e_who_upstreams who1
cat > "$WORK/scenario.yaml" <<'YAML'
port: 8080
host: app.test
healthCheck: true
securityHeaders:
  hstsMaxAgeSecs: 63072000
  csp: "default-src 'none'; frame-ancestors 'none'"
  permissionsPolicy: "camera=(), microphone=(), geolocation=()"
cors:
  origins: ["https://good.test"]
proxy:
  /: "http://who1.test:8080"
YAML
e2e_start_conduit "$WORK/scenario.yaml" http://app.test:8080/
e2e_prove_isolation

mkdir -p "$WORK/zap"
chmod 777 "$WORK/zap"
zap() { docker run --rm --network "$NET" -v "$WORK/zap:/zap/wrk:rw" "$E2E_ZAP" "$@"; }

echo "--- ZAP baseline (passive)"
status=0
zap zap-baseline.py -t http://app.test:8080/ -m 1 -T 3 -J baseline.json -s -z "-silent -config connection.timeoutInSecs=5" || status=$?
# 0 = clean, 1 = at least one FAIL, 2 = WARNs only, 3 = ZAP itself failed: any non-zero is a finding
[ "$status" = 0 ] || fail "ZAP baseline exit status $status (1 = failures, 2 = warnings, 3 = ZAP error); see the alerts above and $WORK/zap/baseline.json"
ok "ZAP baseline: no warning, no failure"

if [ -n "${E2E_ZAP_ACTIVE:-}" ]; then
  echo "--- ZAP full scan (active)"
  status=0
  zap zap-full-scan.py -t http://app.test:8080/ -m 2 -T 10 -J full.json -s -z "-silent -config connection.timeoutInSecs=5" || status=$?
  [ "$status" = 0 ] || fail "ZAP full scan exit status $status"
  ok "ZAP active scan: no warning, no failure"
fi

echo "zap scenario passed"
