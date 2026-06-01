# Deployment Guide

Conduit is a single static binary with no runtime dependencies. It runs on
bare metal, VMs, containers, and Kubernetes.

Not sure what config to write first? **[← Configuration Recipes](recipes.md)**

---

## Table of Contents

- [Docker](#docker)
- [systemd](#systemd)
- [Kubernetes](#kubernetes)
- [Production checklist](#production-checklist)

---

## Docker

Official images are published to the GitHub Container Registry on every release.

```bash
docker pull ghcr.io/lopatnov/conduit:latest
docker pull ghcr.io/lopatnov/conduit:1.0.0
```

The image is a multi-stage musl build packaged into `FROM scratch` (~14 MB).
It runs as UID 65534 (`nobody`) with no shell or OS userland.

### docker run

```bash
docker run -p 8080:8080 \
  -v $(pwd)/conduit.yaml:/etc/conduit/conduit.yaml:ro \
  -v $(pwd)/dist:/dist:ro \
  ghcr.io/lopatnov/conduit -c /etc/conduit/conduit.yaml
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
      - ./certs:/var/cache/conduit/certs
    environment:
      METRICS_TOKEN: "${METRICS_TOKEN}"
      ADMIN_TOKEN:   "${ADMIN_TOKEN}"
    restart: unless-stopped
    healthcheck:
      test: ["CMD-SHELL", "wget -qO- http://localhost:8080/__health__ || exit 1"]
      interval: 10s
      timeout: 3s
      retries: 3

  api:
    image: my-api:latest
    expose: ["4000"]
```

Environment variables referenced as `$VAR` in the config file are substituted
at startup. Pass secrets via `environment:` — never bake them into the image.

### Build your own image

```bash
git clone https://github.com/lopatnov/conduit
cd conduit
docker build -f contrib/Dockerfile -t conduit:local .
```

---

## systemd

The `contrib/conduit.service` unit file is ready for production use. It includes
security hardening (`NoNewPrivileges`, `ProtectSystem=strict`) and automatic
restart on failure.

### Install

```bash
# Copy binary
sudo cp target/release/conduit /usr/local/bin/conduit
sudo chmod 755 /usr/local/bin/conduit

# Create a dedicated user (no home dir, no shell)
sudo useradd --system --no-create-home --shell /usr/sbin/nologin conduit

# Create directories
sudo mkdir -p /etc/conduit /var/log/conduit /var/cache/conduit

# Install config
sudo cp conduit.yaml /etc/conduit/conduit.yaml
sudo chown -R conduit:conduit /etc/conduit /var/log/conduit /var/cache/conduit

# Install service
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

The unit file loads this file automatically via `EnvironmentFile=`. Set
permissions so only the `conduit` user can read it:

```bash
sudo chmod 600 /etc/conduit/conduit.env
sudo chown conduit:conduit /etc/conduit/conduit.env
```

Then reference them in `conduit.yaml`:

```yaml
metrics:
  token: "$METRICS_TOKEN"
jwtAuth:
  secret: "$JWT_SECRET"
```

### Operations

```bash
# Check status
sudo systemctl status conduit

# View logs (live)
sudo journalctl -u conduit -f

# Hot-reload config (no restart, no dropped connections)
sudo systemctl reload conduit
# or via Admin API:
conduit reload --admin 127.0.0.1:2019

# Restart (needed when port or TLS cert/key changes)
sudo systemctl restart conduit

# Graceful shutdown
conduit shutdown --admin 127.0.0.1:2019
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
```

### As a DaemonSet (node-level proxy)

Deploy one Conduit per node as a lightweight node-local ingress or traffic
forwarder.

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
      hostNetwork: true
      containers:
        - name: conduit
          image: ghcr.io/lopatnov/conduit:latest
          args: ["-c", "/etc/conduit/conduit.yaml"]
          ports:
            - containerPort: 8080
              hostPort: 8080
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

# Update after config change
kubectl create configmap conduit-config \
  --from-file=conduit.yaml=./conduit.yaml \
  --dry-run=client -o yaml | kubectl apply -f -
```

Hot-reload after ConfigMap update (requires Admin API Service):

```bash
# Port-forward to a pod's Admin API
kubectl port-forward pod/myapp-xyz 2019:2019 &
conduit reload --admin 127.0.0.1:2019
```

### ConduitSite CRD (`--features kubernetes`)

> **Requires** `cargo build --features kubernetes`

Instead of a config file or ConfigMap, you can configure Conduit directly via
Kubernetes custom resources. Each `ConduitSite` object maps to one site in the
Conduit config. Conduit watches for `Added`, `Modified`, and `Deleted` events
and reloads automatically — no `conduit reload` needed.

**Install the CRD:**

```bash
kubectl apply -f contrib/k8s/conduitsite-crd.yaml
```

**Define a site:**

```yaml
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

The `spec` fields mirror the Conduit config schema exactly — any field valid
in `conduit.yaml` is valid in the `spec`. Multiple `ConduitSite` resources in
a namespace are combined into a single multi-site config.

**Start Conduit in Kubernetes mode:**

```bash
# Watch ConduitSite CRDs in the "default" namespace
conduit --kubernetes-namespace default

# Watch all namespaces (requires cluster-wide RBAC)
conduit --kubernetes-namespace '*'
```

Conduit reads all `ConduitSite` resources in the given namespace, combines them
into a single multi-site config, and hot-reloads whenever a CRD is added,
modified, or deleted — no `conduit reload` needed.

No config file is read when `--kubernetes-namespace` is set. The `-c` flag is
ignored in this mode.

### Compared to nginx-ingress

| Feature | nginx-ingress | Conduit |
| ------- | ------------- | ------- |
| TLS termination | ✅ | ✅ |
| Auto-TLS (ACME) | via cert-manager | ✅ built-in |
| Path-based routing | ✅ | ✅ |
| Load balancing | ✅ | ✅ 8 strategies |
| Rate limiting | via annotations | ✅ native |
| JWT / API key auth | via plugin | ✅ native |
| Prometheus metrics | via exporter | ✅ native |
| Config format | YAML annotations | JSON / YAML file or CRDs |
| Kubernetes CRDs | ✅ | ✅ `--features kubernetes` |
| Binary size | ~50 MB | ~14 MB |

Conduit is a good fit when you want a **simple, self-contained proxy** without
the overhead of a full Ingress controller. For large clusters with many teams
and services, a dedicated controller (nginx, Traefik, Envoy Gateway) scales
better.

---

## Production checklist

Before going live:

```bash
# 1. Validate config — exits 0 if OK, 1 with field-level errors
conduit validate -c conduit.yaml

# 2. Probe all upstreams — exits 1 if any are unreachable
conduit probe -c conduit.yaml

# 3. Check TLS cert expiry and ACME status
conduit validate -c conduit.yaml   # shows cert expiry in output
```

**Config checklist:**

- [ ] `port: 443` with `tls` + `httpRedirectPort: 80` for HTTPS
- [ ] `securityHeaders: true` (or custom values)
- [ ] `logging: json` for structured log ingestion
- [ ] `metrics.token` set to prevent unauthenticated scraping
- [ ] `global.admin.token` set if Admin API is used
- [ ] All secrets in environment variables (`"$VAR"`), not hardcoded
- [ ] `healthCheck: true` for readiness/liveness probes
- [ ] `maskErrors: true` to hide internal stack traces
- [ ] `rateLimit` configured to protect public endpoints
- [ ] `limits.maxInflightRequests` set to cap concurrent load
- [ ] `retry.budgetPercent` set to prevent retry storms
- [ ] `outlierDetection` enabled for passive health tracking
