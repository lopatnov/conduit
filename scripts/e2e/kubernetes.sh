#!/usr/bin/env bash
# Scenario: Conduit reading its sites from ConduitSite objects of a real Kubernetes API server (issue #764).
#
#   scripts/e2e/kubernetes.sh BINARY      (or `run.sh BINARY kubernetes`; the binary needs `--features kubernetes,redis`)
#
# run.sh does not pick this scenario by itself: it needs a binary with the kubernetes feature, so CI runs it in its own
# job (`e2e-kubernetes`). The cluster is k3s in one privileged container (rancher/k3s pinned by digest; kubectl is in the
# image, so nothing else is downloaded). Every image the cluster runs is pulled by digest on the host and staged for
# k3s to import before kubelet starts, so the cluster pulls nothing itself. Conduit runs from an image built here around
# BINARY, the way contrib/Dockerfile builds the real one (UID 65534, entrypoint the binary).
#
# What it checks, against docs/kubernetes.md:
#   - the shipped manifests (contrib/k8s/conduitsite-crd.yaml and contrib/k8s/example-site.yaml: two sites, RBAC, a
#     Deployment of two replicas and a Service) apply, and a request through the Service reaches the upstream;
#   - a ConduitSite added, edited and deleted while Conduit runs: the change is live, and no pod restarts during hot
#     changes;
#   - one replica temporarily loses API access, keeps its last config, then reconnects and resyncs the latest objects;
#   - a rollout reloads the current objects, and objects in another namespace stay out of the config;
#   - `status.ready` / `status.message` are written back for a valid and an invalid object;
#   - an invalid edit of a working site keeps its last good version; a new invalid object answers 503 for its host and
#     holds back no other site;
#   - a Role whose apiGroups are not conduit.lopatnov.github.io: the pod exits with `kubernetes provider error` (403);
#   - CONDUIT_REDIS_URL from a Secret: two replicas share one rate limit through Redis.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

[ "$#" -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
e2e_init "$1"

E2E_K3S=rancher/k3s@sha256:d7119ab9e0054692a86a8eda454f2d3370dafc1c3a96b3ace5c5599f0c1e9f15 # v1.33.6-k3s1
E2E_PAUSE=rancher/mirrored-pause@sha256:74c4244427b7312c5b901fe0f67cbc53683d06f4f24c6faee65d4182bf0fa893 # 3.6
E2E_COREDNS=rancher/mirrored-coredns-coredns@sha256:9b9128672209474da07c91439bf15ed704ae05ad918dd6454e5b6ae14e35fee6 # 1.13.1
E2E_REDIS=redis@sha256:7f7d30b7fdd09467bac58198a1f166dd69789d348d4fe859cccb20e0cf287fbe # 7.4-alpine
E2E_CURL=curlimages/curl@sha256:935d9100e9ba842cdb060de42472c7ca90cfe9a7c96e4dacb55e79e560b3ff40 # 8.17.0
K3S="conduit-e2e-k3s-$E2E_ID"
E2E_WHO="${E2E_WHO:-} $K3S"
# the k3s image tags the cluster expects for its own pods, and the names our pods use
TAG_PAUSE=rancher/mirrored-pause:3.6
TAG_COREDNS=rancher/mirrored-coredns-coredns:1.13.1

kc() { docker exec -i "$K3S" kubectl "$@"; }
# curl_svc ARGS...: curl from the client pod to the Service or a Conduit pod IP
curl_svc() { kc exec client -- curl -sS --max-time 10 "$@"; }
code_svc() { curl_svc -o /dev/null -w '%{http_code}' "$@"; }
# Exclude pods from the old ReplicaSet that are still terminating after a rollout.
running_pod_ips() {
  kc get pods -l app=conduit --field-selector=status.phase=Running \
    -o jsonpath='{range .items[*]}{.metadata.deletionTimestamp}|{.status.podIP}{"\n"}{end}' | sed -n 's/^|//p' | tr '\n' ' '
}
running_pod_names() {
  kc get pods -l app=conduit --field-selector=status.phase=Running -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' | tr '\n' ' '
}
running_pod_uids() {
  kc get pods -l app=conduit --field-selector=status.phase=Running -o jsonpath='{range .items[*]}{.metadata.uid}{"\n"}{end}' | sort | tr '\n' ' '
}
# Interrupt one pod's in-cluster API connection; the k3s container is privileged and nsenter applies a rule inside
# only that pod's network namespace.
block_api_for_pod() {
  local pod=$1 container_id pid
  container_id=$(kc get pod "$pod" -o jsonpath='{.status.containerStatuses[0].containerID}' | sed 's#^containerd://##')
  pid=$(docker exec "$K3S" ctr -a /run/k3s/containerd/containerd.sock -n k8s.io tasks ls | awk -v id="$container_id" '$1 == id { print $2 }')
  [[ "$pid" =~ ^[0-9]+$ ]] || fail "could not find the containerd task PID for $pod"
  BLOCKED_PID=$pid
  KUBE_API_IP=$(kc get service kubernetes -o jsonpath='{.spec.clusterIP}')
  docker exec "$K3S" nsenter -t "$BLOCKED_PID" -n iptables -I OUTPUT 1 -d "$KUBE_API_IP" -p tcp --dport 443 -j REJECT --reject-with tcp-reset
}
unblock_api_for_pod() {
  docker exec "$K3S" nsenter -t "$BLOCKED_PID" -n iptables -D OUTPUT -d "$KUBE_API_IP" -p tcp --dport 443 -j REJECT --reject-with tcp-reset
  if docker exec "$K3S" nsenter -t "$BLOCKED_PID" -n iptables -C OUTPUT -d "$KUBE_API_IP" -p tcp --dport 443 -j REJECT --reject-with tcp-reset >/dev/null 2>&1; then
    fail "the API egress block remained after removal"
  fi
  local response
  response=$(docker exec "$K3S" timeout 5 nsenter -t "$BLOCKED_PID" -n telnet "$KUBE_API_IP" 443 2>&1 || true)
  grep -q 'Connected to' <<< "$response" || fail "the Kubernetes API was not reachable after removing the egress block: $response"
}
# wait_for SECONDS DESCRIPTION COMMAND...: poll COMMAND every second until it succeeds
wait_for() {
  local secs=$1 what=$2
  shift 2
  for _ in $(seq 1 "$secs"); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  fail "$what (waited ${secs}s)"
}
restarts() { kc get pods -l app=conduit -o jsonpath='{range .items[*]}{.status.containerStatuses[0].restartCount}{" "}{end}'; }
ready_status() { kc get conduitsite "$1" -o jsonpath='{.status.ready}'; }
message_status() { kc get conduitsite "$1" -o jsonpath='{.status.message}'; }

e2e_cleanup_k8s() {
  local exit_status=$?
  if [ "$exit_status" -ne 0 ] && docker inspect "$K3S" >/dev/null 2>&1; then
    echo "--- pods"
    kc get pods -A -o wide 2>&1 | tail -20 || true
    echo "--- conduit log"
    kc logs -l app=conduit --tail=40 --prefix 2>&1 || true
    echo "--- conduitsites"
    kc get conduitsites -o wide 2>&1 || true
  fi
  e2e_cleanup || true
  exit "$exit_status"
}
trap e2e_cleanup_k8s EXIT

# ── images: pulled by digest on the host, then imported into the cluster under the names it uses ──
# (no client image build: the client pod is curl's own image, so this job needs no package mirror)
for img in "$E2E_K3S" "$E2E_PAUSE" "$E2E_COREDNS" "$E2E_REDIS" "$E2E_CURL" "$E2E_HTTPBIN"; do
  docker image inspect "$img" >/dev/null 2>&1 || docker pull -q "$img" >/dev/null
done
docker tag "$E2E_PAUSE" "$TAG_PAUSE"
docker tag "$E2E_COREDNS" "$TAG_COREDNS"
docker tag "$E2E_HTTPBIN" e2e/httpbin:1
docker tag "$E2E_REDIS" e2e/redis:1
docker tag "$E2E_CURL" e2e/client:1
mkdir -p "$WORK/image"
cp "$BIN" "$WORK/image/conduit"
cat > "$WORK/image/Dockerfile" <<EOF
FROM $CONDUIT_IMAGE
COPY conduit /conduit
ENV CONDUIT_CONFIG=/etc/conduit/conduit.json
USER 65534:65534
ENTRYPOINT ["/conduit"]
EOF
docker build -q -t e2e/conduit:1 "$WORK/image" >/dev/null

# K3s imports archives from this directory before starting kubelet. Stage every image in one tarball so the bundled
# CoreDNS workload cannot begin its IfNotPresent pull before its pinned image is imported.
mkdir -p "$WORK/k3s-images"
docker save -o "$WORK/k3s-images/e2e-images.tar" \
  "$TAG_PAUSE" "$TAG_COREDNS" e2e/httpbin:1 e2e/redis:1 e2e/client:1 e2e/conduit:1 >/dev/null

# ── the cluster ──
docker run -d --name "$K3S" --privileged \
  -v "$WORK/k3s-images:/var/lib/rancher/k3s/agent/images:ro" "$E2E_K3S" server \
  --disable traefik --disable metrics-server --disable local-storage >/dev/null
wait_for 120 "the k3s API server does not answer" kc get --raw /readyz
wait_for 120 "the node is not Ready" sh -c "docker exec $K3S kubectl get nodes --no-headers | grep -qw Ready"
wait_for 120 "the default service account does not exist" kc get serviceaccount default
ok "k3s is up, every image is imported"

# the upstreams the example sites name (api-svc:4000, admin-svc:5000) are one go-httpbin; a client pod sends the requests
kc apply -f - >/dev/null <<'YAML'
apiVersion: apps/v1
kind: Deployment
metadata: { name: httpbin, namespace: default }
spec:
  selector: { matchLabels: { app: httpbin } }
  template:
    metadata: { labels: { app: httpbin } }
    spec:
      containers:
        - { name: httpbin, image: e2e/httpbin:1, imagePullPolicy: Never, ports: [{ containerPort: 8080 }] }
---
apiVersion: v1
kind: Service
metadata: { name: api-svc, namespace: default }
spec: { selector: { app: httpbin }, ports: [{ port: 4000, targetPort: 8080 }] }
---
apiVersion: v1
kind: Service
metadata: { name: admin-svc, namespace: default }
spec: { selector: { app: httpbin }, ports: [{ port: 5000, targetPort: 8080 }] }
---
apiVersion: v1
kind: Pod
metadata: { name: client, namespace: default }
spec:
  containers:
    - { name: client, image: e2e/client:1, imagePullPolicy: Never, command: [sleep, infinity] }
YAML

# ── 1. the shipped manifests ──
kc apply -f - >/dev/null < "$repo/contrib/k8s/conduitsite-crd.yaml"
kc wait --for=condition=Established --timeout=60s crd/conduitsites.conduit.lopatnov.github.io >/dev/null
# the example as shipped, with the image built here instead of the published one
sed 's#ghcr.io/lopatnov/conduit:latest#e2e/conduit:1#' "$repo/contrib/k8s/example-site.yaml" > "$WORK/example-site.yaml"
grep -q 'e2e/conduit:1' "$WORK/example-site.yaml" || fail "contrib/k8s/example-site.yaml no longer names ghcr.io/lopatnov/conduit:latest"
kc apply -f - >/dev/null < "$WORK/example-site.yaml"
kc rollout status deployment/conduit --timeout=180s >/dev/null || fail "the Deployment of contrib/k8s/example-site.yaml did not become ready"
kc wait --for=condition=Ready --timeout=120s pod/client >/dev/null
kc rollout status deployment/httpbin --timeout=120s >/dev/null
[ "$(kc get deployment conduit -o jsonpath='{.status.readyReplicas}')" = 2 ] || fail "not both replicas of conduit are ready"
ok "contrib/k8s: the CRD, the RBAC, the Deployment (2 replicas, readiness on /__health__) and the Service apply"

out=$(curl_svc -H 'Host: app.example.com' http://conduit/api/anything/x)
echo "$out" | grep -q '"url": *"[^"]*/anything/x"' || fail "app.example.com/api through the Service did not reach the upstream (stripPrefix): $out"
[ "$(code_svc -H 'Host: admin.example.com' http://conduit/get)" = 200 ] || fail "admin.example.com through the Service: $(code_svc -H 'Host: admin.example.com' http://conduit/get)"
[ "$(ready_status my-app)" = true ] || fail "status.ready of my-app is '$(ready_status my-app)', expected true"
[ "$(message_status my-app)" = Valid ] || fail "status.message of my-app is '$(message_status my-app)', expected Valid"
ok "requests through the Service reach the upstream of each example site; status.ready=true, message Valid"

# ── 2. a site added, edited and deleted while Conduit runs ──
before=$(restarts)
# Give unmatched hosts a stable response regardless of the order of the example sites.
kc apply -f - >/dev/null <<'YAML'
apiVersion: conduit.lopatnov.github.io/v1
kind: ConduitSite
metadata: { name: e2e-unmatched, namespace: default }
spec:
  port: 8080
  healthCheck: true
  fallback: { status: 404 }
YAML
pod_ips=$(running_pod_ips)
read -r -a pods <<< "$pod_ips"
[ "${#pods[@]}" -eq 2 ] || fail "expected two running conduit pods, found ${#pods[@]}"
live_site() { # live_site VERSION [EXTRA_SPEC_LINE]
  kc apply -f - >/dev/null <<YAML
apiVersion: conduit.lopatnov.github.io/v1
kind: ConduitSite
metadata: { name: e2e-live, namespace: default }
spec:
  port: 8080
  host: live.test
  proxy: http://api-svc:4000
  headers: { X-E2E-Version: "$1" }
${2:-}
YAML
}
version_everywhere() {
  local ip headers
  for ip in "${pods[@]}"; do
    headers=$(curl_svc -fI -H 'Host: live.test' "http://$ip:8080/get") || return 1
    grep -Fxi "x-e2e-version: $1" <<< "${headers//$'\r'/}" >/dev/null || return 1
  done
}
version_on_ip() {
  local ip=$1 version=$2 headers
  headers=$(curl_svc -fI -H 'Host: live.test' "http://$ip:8080/get") || return 1
  printf '%s\n' "$headers" | tr -d '\r' | grep -Fxi "x-e2e-version: $version" >/dev/null
}
unmatched() {
  local code
  code=$(code_svc -H 'Host: live.test' "$1") || return 1
  [ "$code" = 404 ]
}
wait_for 60 "live.test did not return HTTP 404 through the Service before creation" unmatched http://conduit/get
live_site 1
wait_for 60 "a new ConduitSite was not served" version_everywhere 1
wait_for 30 "status.ready of a new valid site" sh -c "[ \"\$(docker exec $K3S kubectl get conduitsite e2e-live -o jsonpath='{.status.ready}')\" = true ]"
ok "a ConduitSite added at run time is served on both replicas, status.ready=true"

live_site 2
wait_for 60 "an edit of a ConduitSite did not take effect" version_everywhere 2
ok "an edit is live (X-E2E-Version 1 -> 2)"

# Interrupt one replica's API connection, then update the CRD. The connected replica must update while the isolated
# replica keeps its last config. After API access returns, the watcher must reconnect and resync the current list.
blocked_pod=$(awk '{print $1}' <<< "$(running_pod_names)")
blocked_ip=$(kc get pod "$blocked_pod" -o jsonpath='{.status.podIP}')
healthy_ip=$(awk -v blocked="$blocked_ip" '{for (i = 1; i <= NF; i++) if ($i != blocked) {print $i; exit}}' <<< "$pod_ips")
[ -n "$healthy_ip" ] || fail "could not find a second conduit pod to compare with $blocked_pod"
block_api_for_pod "$blocked_pod"
live_site 3
wait_for 60 "the replica with API access did not apply the new site version" version_on_ip "$healthy_ip" 3
version_on_ip "$blocked_ip" 2 || fail "the isolated replica changed config without access to the Kubernetes API"
unblock_api_for_pod
wait_for 120 "the isolated replica did not resync the current site version after API access returned" version_everywhere 3
ok "a replica recovered from Kubernetes API/watch interruption and resynced the latest site version"

# an edit that breaks the site (a host path is refused in a ConduitSite): the last good version keeps serving
live_site 4 '  static: /etc'
wait_for 30 "status.ready of an invalid edit is not false" sh -c "[ \"\$(docker exec $K3S kubectl get conduitsite e2e-live -o jsonpath='{.status.ready}')\" = false ]"
msg=$(message_status e2e-live)
echo "$msg" | grep -qi 'host file path' || fail "status.message of an invalid edit does not say why: $msg"
sleep 3
version_everywhere 3 || fail "after an invalid edit the site does not serve its last good version"
ok "an invalid edit: status.ready=false with the reason, the last good version keeps serving"

# a new invalid object: its host answers 503 (the placeholder), the other sites are not held back
kc apply -f - >/dev/null <<'YAML'
apiVersion: conduit.lopatnov.github.io/v1
kind: ConduitSite
metadata: { name: e2e-bad, namespace: default }
spec:
  port: 8080
  host: bad.test
  proxy: http://api-svc:4000
  static: /etc
YAML
wait_for 60 "a new invalid ConduitSite does not answer 503 for its host" sh -c "[ \"\$(docker exec $K3S kubectl exec client -- curl -sS --max-time 10 -o /dev/null -w '%{http_code}' -H 'Host: bad.test' http://conduit/etc/passwd)\" = 503 ]"
[ "$(ready_status e2e-bad)" = false ] || fail "status.ready of a new invalid object is '$(ready_status e2e-bad)'"
[ "$(code_svc -H 'Host: app.example.com' http://conduit/api/get)" = 200 ] || fail "a new invalid object held back another site"
version_everywhere 3 || fail "a new invalid object held back e2e-live"
ok "a new invalid ConduitSite: 503 for its host, status.ready=false, the other sites still served"

# Restore a valid current CRD before restarting, so the new pods must load its latest version from the API.
live_site 3
wait_for 60 "the valid site edit was not restored" version_everywhere 3
wait_for 30 "status.ready of the restored valid site" sh -c "[ \"\$(docker exec $K3S kubectl get conduitsite e2e-live -o jsonpath='{.status.ready}')\" = true ]"

# A resource in another namespace is outside this deployment's watch scope.
kc create namespace e2e-other >/dev/null
kc apply -f - >/dev/null <<'YAML'
apiVersion: conduit.lopatnov.github.io/v1
kind: ConduitSite
metadata: { name: e2e-other-namespace, namespace: e2e-other }
spec:
  port: 8080
  host: other-namespace.test
  proxy: http://api-svc:4000
YAML
namespace_site_unmatched() {
  local ip code
  for ip in "${pods[@]}"; do
    code=$(code_svc -H 'Host: other-namespace.test' "http://$ip:8080/get") || return 1
    [ "$code" = 404 ] || return 1
  done
}
wait_for 60 "a ConduitSite from another namespace affected the default namespace config" namespace_site_unmatched
other_namespace_status=$(kc get conduitsite e2e-other-namespace -n e2e-other -o jsonpath='{.status.ready}')
[ -z "$other_namespace_status" ] || fail "Conduit wrote status for a ConduitSite outside its watched namespace"

[ "$(restarts)" = "$before" ] || fail "a conduit pod restarted during the live changes ($before -> $(restarts))"
ok "no pod restarted during live changes"

# Restart both pods and prove they load the latest valid object, keep invalid objects inactive, and still ignore
# ConduitSites from outside their configured namespace.
old_uids=$(running_pod_uids)
kc rollout restart deployment/conduit >/dev/null
kc rollout status deployment/conduit --timeout=180s >/dev/null || fail "conduit did not recover after a rollout restart"
new_uids=$(running_pod_uids)
[ "$old_uids" != "$new_uids" ] || fail "the rollout restart did not replace the conduit pods"
pod_ips=$(running_pod_ips)
read -r -a pods <<< "$pod_ips"
[ "$(wc -w <<< "$pod_ips")" -eq 2 ] || fail "expected two running conduit pods after restart"
wait_for 60 "new conduit pods did not load the latest valid ConduitSite" version_everywhere 3
[ "$(ready_status e2e-live)" = true ] || fail "the restarted pods did not report the valid ConduitSite as ready"
[ "$(ready_status e2e-bad)" = false ] || fail "the restarted pods did not report the invalid ConduitSite"
namespace_site_unmatched || fail "a restarted pod loaded a ConduitSite from another namespace"
other_namespace_status=$(kc get conduitsite e2e-other-namespace -n e2e-other -o jsonpath='{.status.ready}')
[ -z "$other_namespace_status" ] || fail "restarted Conduit pods wrote status outside their watched namespace"
ok "new pods loaded the current config and kept other namespaces isolated"

# delete: the host is not served any more
kc delete conduitsite e2e-live e2e-bad >/dev/null
gone() {
  local ip
  for ip in "${pods[@]}"; do
    unmatched "http://$ip:8080/get" || return 1
  done
}
wait_for 60 "a deleted ConduitSite is still served" gone
ok "a deleted ConduitSite is no longer served"

# ── 3. a Role without the new apiGroups: the pod says why and exits ──
kc apply -f - >/dev/null <<'YAML'
apiVersion: v1
kind: ServiceAccount
metadata: { name: conduit-old-group, namespace: default }
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata: { name: conduit-old-group, namespace: default }
rules:
  - { apiGroups: [conduit.io], resources: [conduitsites], verbs: [get, list, watch] }
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata: { name: conduit-old-group, namespace: default }
subjects: [{ kind: ServiceAccount, name: conduit-old-group, namespace: default }]
roleRef: { kind: Role, name: conduit-old-group, apiGroup: rbac.authorization.k8s.io }
---
apiVersion: v1
kind: Pod
metadata: { name: conduit-old-group, namespace: default }
spec:
  serviceAccountName: conduit-old-group
  restartPolicy: Never
  containers:
    - { name: conduit, image: e2e/conduit:1, imagePullPolicy: Never, args: [--kubernetes-namespace=default] }
YAML
wait_for 60 "a pod without RBAC for conduit.lopatnov.github.io did not exit" sh -c "docker exec $K3S kubectl get pod conduit-old-group -o jsonpath='{.status.phase}' | grep -q Failed"
log=$(kc logs conduit-old-group 2>&1)
echo "$log" | grep -q 'kubernetes provider error' || fail "the pod exited without 'kubernetes provider error': $log"
echo "$log" | grep -qi 'forbidden\|403' || fail "the error does not say the API refused (403): $log"
ok "a Role for the old group conduit.io: the pod exits with 'kubernetes provider error' (403 forbidden)"

# ── 4. CONDUIT_REDIS_URL from a Secret: one limit shared by both replicas ──
kc apply -f - >/dev/null <<'YAML'
apiVersion: apps/v1
kind: Deployment
metadata: { name: redis, namespace: default }
spec:
  selector: { matchLabels: { app: redis } }
  template:
    metadata: { labels: { app: redis } }
    spec:
      containers:
        - { name: redis, image: e2e/redis:1, imagePullPolicy: Never, ports: [{ containerPort: 6379 }] }
---
apiVersion: v1
kind: Service
metadata: { name: redis, namespace: default }
spec: { selector: { app: redis }, ports: [{ port: 6379 }] }
---
apiVersion: v1
kind: Secret
metadata: { name: conduit-redis, namespace: default }
stringData: { url: "redis://redis.default.svc:6379" }
---
apiVersion: conduit.lopatnov.github.io/v1
kind: ConduitSite
metadata: { name: e2e-limited, namespace: default }
spec:
  port: 8080
  host: limited.test
  proxy: http://api-svc:4000
  rateLimit: { windowSecs: 120, limit: 6, keyBy: ip, store: redis, skipPaths: [/status/200] }
YAML
kc rollout status deployment/redis --timeout=120s >/dev/null
# the address is read at start: set it from the Secret and roll the pods
kc patch deployment conduit --type=json -p '[{"op":"add","path":"/spec/template/spec/containers/0/env","value":[{"name":"CONDUIT_REDIS_URL","valueFrom":{"secretKeyRef":{"name":"conduit-redis","key":"url"}}}]}]' >/dev/null
kc rollout status deployment/conduit --timeout=180s >/dev/null || fail "conduit did not come back with CONDUIT_REDIS_URL from the Secret"
wait_for 60 "limited.test is not served after the rollout" sh -c "[ \"\$(docker exec $K3S kubectl exec client -- curl -sS --max-time 10 -o /dev/null -w '%{http_code}' -H 'Host: limited.test' http://conduit/status/200)\" = 200 ]"
# 12 requests, sent to each replica in turn by pod address (not through the Service, whose choice is random): with one
# bucket per replica each would serve its 6 and all 12 would pass; the shared bucket serves only the first 6.
# (The wait above asks for /status/200, which the limit skips, so it spends nothing.)
pod_ips=$(running_pod_ips)
read -r -a pods <<< "$pod_ips"
[ "${#pods[@]}" -eq 2 ] || fail "expected two running conduit pods, found ${#pods[@]}"
for i in $(seq 1 12); do
  ip=${pods[$((i % 2))]}
  expected=200
  if [ "$i" -gt 6 ]; then expected=429; fi
  code=$(code_svc -H 'Host: limited.test' "http://$ip:8080/get") || fail "rate-limit request $i to $ip failed"
  [ "$code" = "$expected" ] || fail "rate-limit request $i to $ip returned HTTP $code, expected $expected"
done
keys=$(kc exec deploy/redis -- redis-cli --scan | wc -l)
[ "$keys" -ge 1 ] || fail "the Redis holds no rate-limit key"
ok "CONDUIT_REDIS_URL from a Secret: two replicas served 6 of 12 requests (limit 6), the buckets are in Redis"

echo "kubernetes scenario passed"
