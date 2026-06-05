# Deployment Guide

Conduit is a single static binary with no runtime dependencies. It runs on
bare metal, VMs, containers, and Kubernetes.

Not sure what config to write first? **[← Configuration Recipes](recipes.md)**

---

## Table of Contents

- [Docker](#docker)
  - [docker run](#docker-run)
  - [docker-compose](#docker-compose)
  - [Build your own image](#build-your-own-image)
- [systemd](#systemd)
  - [Install](#install)
  - [Secrets](#secrets)
  - [Unit file](#unit-file)
  - [Operations](#operations)
  - [Log rotation](#log-rotation)
- [Kubernetes](#kubernetes)
  - [As a sidecar proxy](#as-a-sidecar-proxy)
  - [As a DaemonSet (node-level proxy)](#as-a-daemonset-node-level-proxy)
  - [ConfigMap](#configmap)
  - [ConduitSite CRD (`--features kubernetes`)](#conduitsite-crd----features-kubernetes)
  - [Compared to nginx-ingress](#compared-to-nginx-ingress)
- [Updating the binary](#updating-the-binary)
- [Production checklist](#production-checklist)

---

## Docker

Two image variants are published to the GitHub Container Registry on every release:

| Variant  | Tags                                       | Includes                                                                                                                                                         |
| -------- | ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Standard | `:latest`, `:1.1.0`, `:1.1`                | Core proxy — no optional features. Smallest binary (~14 MB)                                                                                                      |
| Full     | `:latest-full`, `:1.1.0-full`, `:1.1-full` | All 14 optional features: `jwt`, `consumers`, `forward-auth`, `rhai`, `wasm`, `tcp`, `upload`, `redis`, `cache`, `disk-cache`, `acme`, `fault-injection`, `otlp`, `kubernetes` |

```bash
# Standard (~14 MB)
docker pull ghcr.io/lopatnov/conduit:latest

# Full — all optional features enabled
docker pull ghcr.io/lopatnov/conduit:latest-full
```

> **Standard vs Full:** the standard image covers the majority of production use
> cases — TLS, proxying, routing, rate limiting, basic/API-key auth, metrics,
> hot-reload. Use the full image (or build from source with `--features`) when
> you need JWT validation, scripting middleware (Rhai/WASM), auto-TLS (ACME),
> file uploads, Redis-backed rate limiting, or Kubernetes CRD config.

Both images are multi-stage musl builds packaged into `FROM scratch`.
They run as UID 65534 (`nobody`) with no shell or OS userland.

> **Healthchecks in FROM scratch images:** because the image has no shell or
> system utilities (`wget`, `curl`), Docker `CMD-SHELL` healthchecks do not
> work. Use `conduit status` via the Admin API (requires `global.admin` in
> config), or rely on Kubernetes `httpGet` probes which are executed externally
> by kubelet — not inside the container.

### docker run

```bash
docker run -p 8080:8080 \
  -v /path/to/conduit.yaml:/etc/conduit/conduit.yaml:ro \
  ghcr.io/lopatnov/conduit:latest -c /etc/conduit/conduit.yaml
```

For TLS, also mount cert files:

```bash
docker run -p 443:443 -p 80:80 \
  -v /path/to/conduit.yaml:/etc/conduit/conduit.yaml:ro \
  -v /etc/tls:/etc/tls:ro \
  ghcr.io/lopatnov/conduit:latest -c /etc/conduit/conduit.yaml
```

### docker-compose

```yaml
services:
  conduit:
    image: ghcr.io/lopatnov/conduit:latest
    ports:
      - "443:443"
      - "80:80"
    volumes:
      - ./conduit.yaml:/etc/conduit/conduit.yaml:ro
      - ./dist:/dist:ro
      - ./certs:/var/cache/conduit/certs   # ACME cert storage
    environment:
      METRICS_TOKEN: "${METRICS_TOKEN}"
      ADMIN_TOKEN: "${ADMIN_TOKEN}"
    restart: unless-stopped
    # NOTE: FROM scratch has no shell/wget — use conduit status if Admin API
    # is configured, or rely on external healthchecks (load balancer, K8s probe)
    healthcheck:
      test: ["CMD", "/conduit", "status", "--admin", "127.0.0.1:2019"]
      interval: 10s
      timeout: 3s
      retries: 3
      start_period: 5s

  api:
    image: my-api:latest
    expose: ["4000"]
```

> The `healthcheck` above requires `global.admin.bind: "127.0.0.1:2019"` in
> `conduit.yaml`. Remove the healthcheck block if the Admin API is not configured.

Environment variables referenced as `$VAR` in the config file are substituted
at startup. Pass secrets via `environment:` — never bake them into the image.

### Build your own image

```bash
git clone https://github.com/lopatnov/conduit
cd conduit

# Standard image
docker build -f contrib/Dockerfile -t conduit:local .

# Full-features image
docker build -f contrib/Dockerfile \
  --build-arg FEATURES=full \
  -t conduit:local-full .
```

---

## systemd

The `contrib/conduit.service` unit file is ready for production use. It includes
security hardening (`NoNewPrivileges`, `ProtectSystem=strict`) and automatic
restart on failure.

### Install

**Step 1 — Get the binary.**

Download a pre-built binary from [GitHub Releases](https://github.com/lopatnov/conduit/releases):

```bash
# Linux x86-64 (standard)
curl -L https://github.com/lopatnov/conduit/releases/latest/download/conduit-x86_64-unknown-linux-gnu.tar.gz \
  | tar xz
sudo install -m 755 conduit /usr/local/bin/conduit

# Linux x86-64 (full — JWT, WASM, OTLP, etc.)
curl -L https://github.com/lopatnov/conduit/releases/latest/download/conduit-x86_64-unknown-linux-gnu-full.tar.gz \
  | tar xz
sudo install -m 755 conduit /usr/local/bin/conduit
```

Or build from source — see [docs/building.md](building.md).

**Step 2 — Create user, directories, and install service:**

```bash
# Create a dedicated system user (no home dir, no shell)
sudo useradd --system --no-create-home --shell /usr/sbin/nologin conduit

# Create directories
sudo mkdir -p /etc/conduit /var/log/conduit /var/cache/conduit

# Install config (edit before starting)
sudo cp conduit.yaml /etc/conduit/conduit.yaml
sudo chown -R conduit:conduit /etc/conduit /var/log/conduit /var/cache/conduit

# Install and enable service
sudo cp contrib/conduit.service /etc/systemd/system/conduit.service
sudo systemctl daemon-reload
sudo systemctl enable --now conduit
```

### Secrets

Put environment variables (tokens, passwords, keys) in `/etc/conduit/conduit.env`:

```bash
# /etc/conduit/conduit.env
METRICS_TOKEN=my-scrape-token
ADMIN_TOKEN=my-admin-token
JWT_SECRET=my-jwt-secret
API_KEY=my-api-key
```

Set permissions so only the `conduit` user can read it:

```bash
sudo chmod 600 /etc/conduit/conduit.env
sudo chown conduit:conduit /etc/conduit/conduit.env
```

Reference them in `conduit.yaml`:

```yaml
metrics:
  token: "$METRICS_TOKEN"
jwtAuth:
  secret: "$JWT_SECRET"
```

The unit file loads `conduit.env` automatically via `EnvironmentFile=`.

### Unit file

Key parts of `contrib/conduit.service`:

```ini
[Service]
User=conduit
Group=conduit
WorkingDirectory=/etc/conduit
ExecStart=/usr/local/bin/conduit -c /etc/conduit/conduit.yaml
ExecReload=/usr/local/bin/conduit reload   # ← enables `systemctl reload`
Restart=on-failure
RestartSec=5s
LimitNOFILE=65536

# Hardening
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/log/conduit /var/cache/conduit

EnvironmentFile=-/etc/conduit/conduit.env
```

`ExecReload` wires `systemctl reload conduit` to a zero-downtime hot-reload via
the Admin API — no connections are dropped.

### Operations

```bash
# Check status
sudo systemctl status conduit

# View logs (live)
sudo journalctl -u conduit -f

# Hot-reload config (zero dropped connections)
sudo systemctl reload conduit
# or directly:
conduit reload --admin 127.0.0.1:2019

# Restart (required when port, TLS cert/key, or workers change)
sudo systemctl restart conduit

# Graceful shutdown
conduit shutdown --admin 127.0.0.1:2019

# Check upstream health
conduit status --upstream --admin 127.0.0.1:2019
```

### Log rotation

```ini
# /etc/logrotate.d/conduit
/var/log/conduit/*.log {
    daily
    rotate 30
    compress
    delaycompress
    missingok
    notifempty
    postrotate
        # Reload to switch to the new log file atomically.
        # If Admin API is on a non-default port, update the --admin flag.
        conduit reload --admin 127.0.0.1:2019 || true
    endscript
}
```

---

## Kubernetes

Conduit is not a Kubernetes-native ingress controller, but it fits well in
several deployment patterns.

### As a sidecar proxy

Run Conduit alongside your application container. Conduit handles TLS
termination, auth, rate limiting, and static file serving; your app handles
business logic only.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: myapp
spec:
  replicas: 3
  selector:
    matchLabels:
      app: myapp
  template:
    metadata:
      labels:
        app: myapp
    spec:
      initContainers:
        # Copy frontend build into shared volume
        - name: copy-static
          image: myapp-frontend:latest
          command: ["cp", "-r", "/build/.", "/dist"]
          volumeMounts:
            - name: static-files
              mountPath: /dist

      containers:
        - name: api
          image: myapp:latest
          ports:
            - containerPort: 4000

        - name: conduit
          image: ghcr.io/lopatnov/conduit:latest
          args: ["-c", "/etc/conduit/conduit.yaml"]
          ports:
            - name: http
              containerPort: 8080
          volumeMounts:
            - name: conduit-config
              mountPath: /etc/conduit
            - name: static-files
              mountPath: /dist
          readinessProbe:
            httpGet: { path: /__health__, port: 8080 }
            initialDelaySeconds: 2
          livenessProbe:
            httpGet: { path: /__health__, port: 8080 }
            periodSeconds: 10
          env:
            - name: METRICS_TOKEN
              valueFrom:
                secretKeyRef: { name: conduit-secrets, key: metrics-token }

      volumes:
        - name: conduit-config
          configMap:
            name: conduit-config
        - name: static-files
          emptyDir: {}
---
# Expose via a Service
apiVersion: v1
kind: Service
metadata:
  name: myapp
spec:
  selector:
    app: myapp
  ports:
    - name: http
      port: 80
      targetPort: 8080
```

### As a DaemonSet (node-level proxy)

Deploy one Conduit per node as a lightweight node-local ingress or traffic
forwarder.

> **Security note:** `hostNetwork: true` gives the pod full access to the
> node's network namespace. Only use this when necessary (e.g. binding privileged
> ports < 1024 on a node without `CAP_NET_BIND_SERVICE`). Prefer a standard
> Deployment + Service + NodePort for most use cases.

```yaml
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: conduit-proxy
spec:
  selector:
    matchLabels:
      app: conduit-proxy
  template:
    metadata:
      labels:
        app: conduit-proxy
    spec:
      hostNetwork: true   # ← grants node network access; see security note above
      containers:
        - name: conduit
          image: ghcr.io/lopatnov/conduit:latest
          args: ["-c", "/etc/conduit/conduit.yaml"]
          ports:
            - containerPort: 8080
              hostPort: 8080
          readinessProbe:
            httpGet: { path: /__health__, port: 8080 }
            initialDelaySeconds: 2
          volumeMounts:
            - name: config
              mountPath: /etc/conduit
      volumes:
        - name: config
          configMap:
            name: conduit-config
```

### ConfigMap

Store the config as a ConfigMap so it can be updated without rebuilding images.
Reference secrets from Kubernetes Secrets using `valueFrom.secretKeyRef`.

```bash
# Create ConfigMap from local file
kubectl create configmap conduit-config \
  --from-file=conduit.yaml=./conduit.yaml

# Update after config change (atomic apply)
kubectl create configmap conduit-config \
  --from-file=conduit.yaml=./conduit.yaml \
  --dry-run=client -o yaml | kubectl apply -f -
```

Hot-reload after ConfigMap update (requires Admin API in config):

```bash
# Port-forward to a pod's Admin API, then reload
kubectl port-forward pod/myapp-xyz 2019:2019 &
conduit reload --admin 127.0.0.1:2019
```

> For automatic hot-reload on ConfigMap changes, use the
> [ConduitSite CRD](#conduitsite-crd----features-kubernetes) instead —
> it watches Kubernetes events directly without polling.

### ConduitSite CRD (`--features kubernetes`)

> **Requires** `cargo build --features kubernetes` — use the `:latest-full` Docker image.

Instead of a config file or ConfigMap, configure Conduit with Kubernetes custom
resources. Each `ConduitSite` object maps to one virtual site. Changes are
applied automatically — no `conduit reload` needed.

**How it works:**

1. Conduit connects to the cluster (via `KUBECONFIG` or in-cluster service account)
2. Reads all `ConduitSite` resources in the given namespace
3. Combines them into a running multi-site config
4. Watches `Added` / `Modified` / `Deleted` events → hot-reloads automatically

**Step 1 — Install the CRD definition (once per cluster):**

```bash
kubectl apply -f contrib/k8s/conduitsite-crd.yaml
```

**Step 2 — Create RBAC resources:**

```yaml
# contrib/k8s/rbac.yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: conduit
  namespace: default
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: conduit-crd-reader
  namespace: default
rules:
  - apiGroups: ["conduit.io"]
    resources: ["conduitsites"]
    verbs: ["get", "list", "watch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: conduit-crd-reader
  namespace: default
subjects:
  - kind: ServiceAccount
    name: conduit
roleRef:
  kind: Role
  name: conduit-crd-reader
  apiGroup: rbac.authorization.k8s.io
```

For all-namespaces mode (`--kubernetes-namespace '*'`), use `ClusterRole` and
`ClusterRoleBinding` instead of `Role` and `RoleBinding`.

```bash
kubectl apply -f contrib/k8s/rbac.yaml
```

**Step 3 — Deploy Conduit with the CRD flag:**

```yaml
# conduit-deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: conduit
spec:
  replicas: 1
  selector:
    matchLabels: { app: conduit }
  template:
    metadata:
      labels: { app: conduit }
    spec:
      serviceAccountName: conduit
      containers:
        - name: conduit
          image: ghcr.io/lopatnov/conduit:latest-full
          args: ["--kubernetes-namespace", "default"]
          ports:
            - containerPort: 8080
          readinessProbe:
            httpGet: { path: /__health__, port: 8080 }
```

```bash
conduit --kubernetes-namespace default
conduit --kubernetes-namespace '*'   # all namespaces (requires ClusterRole)
```

**Step 4 — Create ConduitSite resources:**

```yaml
# my-app-site.yaml
apiVersion: conduit.io/v1
kind: ConduitSite
metadata:
  name: my-app
  namespace: default
spec:
  port: 8080
  proxy: "http://my-svc:4000"
  healthCheck: true
  rateLimit:
    windowSecs: 60
    limit: 500
```

```yaml
# my-api-site.yaml — TLS + JWT auth
apiVersion: conduit.io/v1
kind: ConduitSite
metadata:
  name: my-api
  namespace: default
spec:
  port: 443
  host: api.example.com
  tls:
    acme:
      email: admin@example.com
      storage: /var/cache/conduit/certs   # mount a PVC at this path
  jwtAuth:
    jwksUrl: "https://auth.example.com/.well-known/jwks.json"
  proxy:
    /v1: "http://api-svc:4000"
  healthCheck: true
```

```bash
kubectl apply -f my-app-site.yaml
kubectl apply -f my-api-site.yaml
# Conduit detects the new resources and hot-reloads within seconds
```

The `spec` fields mirror the Conduit config schema exactly — every field valid
in `conduit.yaml` works in `spec`.

**Update a site** (zero-downtime):

```bash
kubectl patch conduitsite my-app \
  -p '{"spec":{"rateLimit":{"limit":1000}}}' --type=merge
# Hot-reloads within seconds
```

**Delete a site:**

```bash
kubectl delete conduitsite my-app
```

### Compared to nginx-ingress

| Feature            | nginx-ingress    | Conduit                    |
| ------------------ | ---------------- | -------------------------- |
| TLS termination    | ✅               | ✅                         |
| Auto-TLS (ACME)    | via cert-manager | ✅ built-in                |
| Path-based routing | ✅               | ✅                         |
| Load balancing     | ✅               | ✅ 8 strategies            |
| Rate limiting      | via annotations  | ✅ native                  |
| JWT / API key auth | via plugin       | ✅ native                  |
| Prometheus metrics | via exporter     | ✅ native                  |
| Config format      | YAML annotations | JSON / YAML file or CRDs   |
| Kubernetes CRDs    | ✅               | ✅ `--features kubernetes` |
| Binary size        | ~50 MB           | ~14 MB                     |

Conduit is a good fit when you want a **simple, self-contained proxy** without
the overhead of a full Ingress controller. For large clusters with many teams
and services, a dedicated controller (nginx, Traefik, Envoy Gateway) scales
better.

---

## Updating the binary

### Zero-downtime update (systemd)

Conduit supports in-place binary replacement with no dropped connections:

```bash
# 1. Download the new binary alongside the old one
curl -L https://github.com/lopatnov/conduit/releases/latest/download/conduit-x86_64-unknown-linux-gnu.tar.gz \
  | tar xz
sudo install -m 755 conduit /usr/local/bin/conduit.new

# 2. Atomically replace the binary (rename is atomic on Linux)
sudo mv /usr/local/bin/conduit.new /usr/local/bin/conduit

# 3. Restart to load the new binary
#    Use reload first to pick up any config changes,
#    then restart only if the port/TLS/workers changed.
sudo systemctl restart conduit
```

> Conduit's graceful shutdown (`shutdownTimeoutSecs`) ensures in-flight
> requests complete before the process exits. Set
> `global.shutdownTimeoutSecs: 30` in your config to allow up to 30 s.

### Rolling update (Kubernetes)

```bash
# Update the image tag — Kubernetes handles the rolling restart
kubectl set image deployment/myapp conduit=ghcr.io/lopatnov/conduit:1.1.0
kubectl rollout status deployment/myapp

# Roll back if needed
kubectl rollout undo deployment/myapp
```

---

## Production checklist

**Before first deploy:**

```bash
# 1. Validate config — exits 0 if OK, exits 1 with field-level errors
conduit validate -c conduit.yaml

# 2. Probe all upstreams — exits 1 if any are unreachable
conduit probe -c conduit.yaml
```

**Config checklist:**

- [ ] `port: 443` with `tls` + `httpRedirectPort: 80` for HTTPS
- [ ] `securityHeaders: true` (or custom values)
- [ ] `logging: json` for structured log ingestion
- [ ] `metrics.token` set to prevent unauthenticated scraping
- [ ] `global.admin.token` set if Admin API is exposed
- [ ] All secrets in environment variables (`"$VAR"`), not hardcoded
- [ ] `healthCheck: true` for readiness/liveness probes
- [ ] `maskErrors: true` to hide upstream stack traces
- [ ] `rateLimit` configured on all public endpoints
- [ ] `limits.maxInflightRequests` set to cap concurrent load
- [ ] `limits.maxBodyBytes` set to reject oversized request bodies
- [ ] `retry.budgetPercent` set to prevent retry storms
- [ ] `outlierDetection` enabled for passive health tracking
- [ ] Firewall: only ports 80/443 exposed publicly; port 2019 loopback-only

**After deploy:**

```bash
# Verify server is healthy
conduit status --admin 127.0.0.1:2019

# Verify all upstreams are reachable
conduit status --upstream --admin 127.0.0.1:2019

# Check Prometheus metrics are reachable
curl -H "Authorization: Bearer $METRICS_TOKEN" \
  https://your-host/__metrics__ | head -5
```
