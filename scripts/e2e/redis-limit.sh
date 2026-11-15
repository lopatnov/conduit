#!/usr/bin/env bash
# Scenario: one rate limit shared by two Conduit instances through the operator's Redis (issue #760).
#
#   scripts/e2e/redis-limit.sh BINARY      (or through run.sh; the binary needs `--features redis`)
#
# The address of the Redis is written once, in `global.redis.url`; the site says `store: redis` and names no address.
# Two instances, one real Redis, one client that alternates between them:
#   - control: with `store: memory` each instance has its own bucket, so 8 requests (4 each) are all served. A limit
#     that is not shared would pass the check below, so this is what shows that the check can fail;
#   - with `store: redis` the same 8 requests are served 6 times and refused twice (429) whichever instance they
#     reach, and the Redis holds the buckets;
#   - a site that says `store: redis` with no address anywhere is refused at start, with a message that names the fix.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
E2E_REDIS=redis@sha256:7f7d30b7fdd09467bac58198a1f166dd69789d348d4fe859cccb20e0cf287fbe # 7.4-alpine
REDIS="conduit-e2e-redis-$E2E_ID"
APP2="conduit-e2e-app2-$E2E_ID"
E2E_WHO="${E2E_WHO:-} $REDIS $APP2"
e2e_build_client
docker pull -q "$E2E_REDIS" >/dev/null
e2e_network
e2e_who_upstreams who1
e2e_prove_isolation

docker run -d --name "$REDIS" --network "$NET" --ip 192.0.2.30 --network-alias redis.test "$E2E_REDIS" >/dev/null
up=""
for _ in $(seq 1 40); do
  if [ "$(docker exec "$REDIS" redis-cli ping 2>/dev/null)" = PONG ]; then up=1; break; fi
  sleep 0.5
done
[ -n "$up" ] || fail "redis does not answer"

# config STORE: two instances of this file share nothing but what STORE shares
config() {
  cat > "$WORK/conduit.yaml" <<YAML
global:
  redis:
    url: "redis://redis.test:6379"
sites:
  - port: 8080
    proxy: "http://who1.test:8080"
    healthCheck: true
    rateLimit:
      windowSecs: 60
      limit: 6
      keyBy: ip
      store: $1
YAML
}

# start_pair: both instances on $WORK/conduit.yaml
start_pair() {
  docker rm -f "$APP" "$APP2" >/dev/null 2>&1 || true
  docker run -d --name "$APP" --network "$NET" --ip 192.0.2.20 --network-alias app.test \
    -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" conduit -c /conf/conduit.yaml >/dev/null
  docker run -d --name "$APP2" --network "$NET" --ip 192.0.2.21 --network-alias app2.test \
    -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" conduit -c /conf/conduit.yaml >/dev/null
  local host up
  for host in app.test app2.test; do
    up=""
    for _ in $(seq 1 50); do
      # the health path is outside the rate limit, so waiting does not spend the budget
      if client curl -sS --max-time 5 -o /dev/null "http://$host:8080/__health__" 2>/dev/null; then up=1; break; fi
      sleep 0.5
    done
    [ -n "$up" ] || fail "$host does not answer"
  done
}

# eight requests from ONE client container (one source address), alternating between the instances; prints the codes
eight() {
  client sh -c 'for i in 1 2 3 4 5 6 7 8; do h=app.test; [ $((i % 2)) = 0 ] && h=app2.test; \
    curl -sS --max-time 10 -o /dev/null -w "%{http_code} " "http://$h:8080/"; done'
}

# ── control: a memory store is not shared ──
config memory
start_pair
codes=$(eight)
[ "$codes" = "200 200 200 200 200 200 200 200 " ] \
  || fail "control: two instances with store: memory should serve all 8 requests (4 each), got: $codes"
ok "control: store: memory is per instance (8 of 8 served)"

# ── store: redis, the address from global.redis.url ──
config redis
start_pair
codes=$(eight)
[ "$codes" = "200 200 200 200 200 200 429 429 " ] \
  || fail "store: redis should serve 6 of 8 requests across both instances and refuse 2, got: $codes"
keys=$(docker exec "$REDIS" redis-cli dbsize | tr -dc '0-9')
[ "${keys:-0}" -ge 1 ] || fail "the Redis holds no rate-limit state (dbsize=$keys)"
ok "store: redis is shared: 6 of 8 served across two instances, 2 refused, $keys key(s) in the Redis"

# ── a bare `store: redis` with no address anywhere is refused at start ──
cat > "$WORK/noaddr.yaml" <<'YAML'
sites:
  - port: 8080
    proxy: "http://who1.test:8080"
    rateLimit:
      windowSecs: 60
      limit: 6
      store: redis
YAML
set +e
out=$(docker run --rm --network "$NET" -v "$BIN:/usr/local/bin/conduit:ro" -v "$WORK:/conf:ro" "$CONDUIT_IMAGE" \
  timeout 20 conduit -c /conf/noaddr.yaml 2>&1)
status=$?
set -e
[ "$status" -ne 0 ] && [ "$status" -ne 124 ] || fail "a store: redis with no address started (exit $status): $out"
echo "$out" | grep -q 'global.redis.url' || fail "the refusal does not name global.redis.url: $out"
ok "store: redis without an address is refused at start, naming global.redis.url"
