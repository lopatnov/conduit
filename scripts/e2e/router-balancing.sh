#!/usr/bin/env bash
# Scenario: the router and the balancing strategies, with real clients (issue #704 follow-up).
#
#   scripts/e2e/router-balancing.sh BINARY      (or through run.sh)
#
# Three tiny upstreams (who1, who2, who3: each answers with its own name) behind Conduit. A client counts which one
# answered, so a strategy is judged by what it does on the wire: round-robin spreads evenly, weighted-round-robin follows
# the weights, ip-hash and consistent-hash keep one key on one upstream, least-conn sends a request to the idle one while
# another is busy, a dead upstream is taken out of rotation. The router half checks `routes[]` (path, method, header),
# `stripPrefix` and that the first matching route wins.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"
e2e_build_client
cat > "$WORK/scenario.yaml" <<'YAML'
port: 8080
host: app.test
healthCheck: true
routes:
  - match: { path: /method/**, method: [POST] }
    proxy: { targets: ["http://who3.test:8080"], stripPrefix: true }
  - match: { path: /hdr/**, headers: { X-Pick: "two" } }
    proxy: { targets: ["http://who2.test:8080"], stripPrefix: true }
  - match: { path: /first/** }
    proxy: { targets: ["http://who1.test:8080"], stripPrefix: true }
  - match: { path: /first/never/** }
    proxy: { targets: ["http://who3.test:8080"] }
  - match: { path: /rr/** }
    proxy:
      targets: ["http://who1.test:8080", "http://who2.test:8080", "http://who3.test:8080"]
      strategy: round-robin
      stripPrefix: true
  - match: { path: /wrr/** }
    proxy:
      targets:
        - { url: "http://who1.test:8080", weight: 3 }
        - { url: "http://who2.test:8080", weight: 1 }
      strategy: weighted-round-robin
      stripPrefix: true
  - match: { path: /iph/** }
    proxy:
      targets: ["http://who1.test:8080", "http://who2.test:8080", "http://who3.test:8080"]
      strategy: ip-hash
      hashKey: "header:X-Key"
      stripPrefix: true
  - match: { path: /ch/** }
    proxy:
      targets: ["http://who1.test:8080", "http://who2.test:8080", "http://who3.test:8080"]
      strategy: consistent-hash
      hashKey: "header:X-Key"
      stripPrefix: true
  - match: { path: /lc/** }
    proxy:
      targets: ["http://who1.test:8080", "http://who2.test:8080"]
      strategy: least-conn
      stripPrefix: true
  - match: { path: /rnd/** }
    proxy:
      targets: ["http://who1.test:8080", "http://who2.test:8080", "http://who3.test:8080"]
      strategy: random
      stripPrefix: true
  - match: { path: /p2c/** }
    proxy:
      targets: ["http://who1.test:8080", "http://who2.test:8080", "http://who3.test:8080"]
      strategy: p2c
      stripPrefix: true
  - match: { path: /fail/** }
    proxy:
      targets: ["http://who1.test:8080", "http://who2.test:8080"]
      strategy: round-robin
      stripPrefix: true
      retry: { attempts: 2, conditions: [connection_error] }
      healthCheck: { path: /, intervalSecs: 1, unhealthyThreshold: 1, healthyThreshold: 1 }
YAML
e2e_network
e2e_who_upstreams who1 who2 who3
e2e_start_conduit "$WORK/scenario.yaml" http://app.test:8080/__health__
e2e_prove_isolation
CURL=(curl -sS --max-time 10)
BASE=http://app.test:8080

# hits PATH N [curl args]: the names the upstreams answered with, N requests, counted: "who1=4 who2=4".
hits() {
  local path=$1 n=$2
  shift 2
  client sh -c 'for i in $(seq 1 '"$n"'); do curl -sS --max-time 10 "$@"; done | sort | uniq -c | sort -k2 | awk "{printf \"%s=%s \", \$2, \$1}"' sh "$@" "$BASE$path"
}
count() { # count "who1=4 who2=4" who1 -> 4
  echo "$1" | tr ' ' '\n' | sed -n "s/^$2=//p" | head -1 | grep . || echo 0
}

# ── Router ──
out=$(client "${CURL[@]}" -X POST "$BASE/method/x"); [ "$out" = "who3" ] || fail "POST /method/x -> '$out', expected who3"
out=$(client "${CURL[@]}" "$BASE/method/x" -o /dev/null -w '%{http_code}'); [ "$out" != "200" ] || fail "GET /method/x matched a POST-only route"
ok "a route with a method only matches that method"
out=$(client "${CURL[@]}" -H 'X-Pick: two' "$BASE/hdr/x"); [ "$out" = "who2" ] || fail "X-Pick: two -> '$out'"
out=$(client "${CURL[@]}" -H 'X-Pick: one' "$BASE/hdr/x" -o /dev/null -w '%{http_code}'); [ "$out" != "200" ] || fail "route matched without the header value"
ok "a route with a header only matches that value"
out=$(client "${CURL[@]}" "$BASE/first/never/x"); [ "$out" = "who1" ] || fail "first match did not win: '$out'"
ok "the first matching route wins"
out=$(client "${CURL[@]}" "$BASE/rr/anything/here"); echo "$out" | grep -q '^who' || fail "stripPrefix route: '$out'"
ok "stripPrefix route is served"

# ── Strategies ──
r=$(hits /rr/ 30); ok "round-robin: $r"
for w in who1 who2 who3; do [ "$(count "$r" $w)" = 10 ] || fail "round-robin is not even: $r"; done

r=$(hits /wrr/ 40); ok "weighted-round-robin 3:1: $r"
[ "$(count "$r" who1)" = 30 ] && [ "$(count "$r" who2)" = 10 ] || fail "weights not followed: $r"

for strat in iph ch; do
  for key in alpha beta gamma delta; do
    r=$(hits /$strat/ 12 -H "X-Key: $key")
    [ "$(echo "$r" | wc -w)" = 1 ] || fail "$strat: key $key reached several upstreams: $r"
  done
  seen=$( (for key in alpha beta gamma delta epsilon zeta eta theta iota kappa; do hits /$strat/ 1 -H "X-Key: $key"; done) | tr ' ' '\n' | sed 's/=.*//' | sort -u | wc -l)
  [ "$seen" -ge 2 ] || fail "$strat: ten keys all landed on one upstream"
  ok "$strat: a key sticks to one upstream, ten keys spread over $seen"
done

r=$(hits /rnd/ 90); ok "random: $r"
for w in who1 who2 who3; do [ "$(count "$r" $w)" -ge 10 ] || fail "random starves $w: $r"; done
r=$(hits /p2c/ 90); ok "p2c: $r"
for w in who1 who2 who3; do [ "$(count "$r" $w)" -ge 10 ] || fail "p2c starves $w: $r"; done

# least-conn: one slow request pins an upstream (2 s); the quick ones sent meanwhile must all go to the other one.
client sh -c 'curl -sS --max-time 10 "$0/lc/slow" > /tmp/slow & sleep 0.5; for i in $(seq 1 6); do curl -sS --max-time 10 "$0/lc/fast"; done | sort | uniq -c | tr "\n" " " > /tmp/fast; wait; echo "$(cat /tmp/slow)|$(cat /tmp/fast)"' "$BASE" > "$WORK/lc.out"
slow=$(sed 's/|.*//' "$WORK/lc.out"); fast=$(sed 's/.*|//' "$WORK/lc.out")
ok "least-conn: slow request on $slow, quick ones: $fast"
[ -n "$slow" ] && [ "$(echo "$fast" | wc -w)" = 2 ] && ! echo "$fast" | grep -q "$slow" || fail "least-conn sent quick requests to the busy upstream ($slow): $fast"

# ── Failover: stop who1; with a health check and retry no client request fails ──
docker stop "conduit-e2e-who-who1-$E2E_ID" >/dev/null
sleep 3
r=$(hits /fail/ 12); ok "one upstream stopped: $r"
[ "$(count "$r" who2)" = 12 ] || fail "requests were not all served by the live upstream: $r"

echo "router-balancing scenario passed"
