# Deployment Guide

Conduit is a single static binary with no runtime dependencies. It runs anywhere: bare
metal, VMs, Docker, systemd, Kubernetes.

---

## Use Cases

### Static file server / CDN edge

Serve a built frontend (React, Vue, Angular, Svelte) with proper caching headers,
brotli/gzip compression, and dotfile protection.

```json
{
  "port": 8080,
  "static": "./dist",
  "staticOptions": { "maxAge": "7d", "preCompressed": true },
  "compression": true,
  "healthCheck": true
}
```

### SPA with API backend

The most common use case: serve `index.html` for all unknown paths while proxying
`/api` to your backend. The `byAccept` fallback returns JSON errors to API clients
and the SPA shell to browsers.

```json
{
  "port": 3000,
  "static": "./dist",
  "proxy": { "/api": "http://localhost:4000" },
  "fallback": {
    "byAccept": {
      "html": { "status": 200, "file": "./dist/index.html" },
      "json": { "status": 404, "body": { "error": "Not Found" } }
    }
  }
}
```

### API gateway / microservices

Route traffic to different upstream services by path prefix. Add rate limiting, IP
filtering, and auth at the gateway layer so individual services don't need to
implement them.

```json
{
  "port": 8080,
  "rateLimit": { "windowSecs": 60, "limit": 500, "keyBy": "ip" },
  "proxy": {
    "/users":   "http://users-svc:4001",
    "/orders":  "http://orders-svc:4002",
    "/catalog": ["http://catalog1:4003", "http://catalog2:4003"]
  },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__" }
}
```

### Load balancer

Distribute traffic across multiple backend instances with health checks and automatic
failover. Use `weighted-round-robin` when backends have different capacities.

See [`examples/load-balanced.json`](../examples/load-balanced.json) for a full example
with weighted RR, IP-hash, and least-conn strategies.

### Development server

Hot-reload in the browser whenever source files change — no build tool or browser
extension needed.

```json
{
  "port": 3000,
  "logging": "dev",
  "cors": true,
  "hotReload": { "extensions": [".html", ".css", ".js", ".ts"] },
  "static": "./src",
  "proxy": { "/api": "http://localhost:4000" }
}
```

---

## Docker

Official images are published to the GitHub Container Registry on every release:

```bash
docker pull ghcr.io/lopatnov/conduit:latest
docker pull ghcr.io/lopatnov/conduit:1.0.0
```

The image is built from a multi-stage Alpine → scratch pipeline (~14 MB), runs as
UID 65534 (nobody), and has no shell or OS userland.

### Run

```bash
docker run -p 8080:8080 \
  -v $(pwd)/conduit.json:/etc/conduit/conduit.json:ro \
  -v $(pwd)/dist:/dist:ro \
  ghcr.io/lopatnov/conduit
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
      - ./conduit.json:/etc/conduit/conduit.json:ro
      - ./dist:/dist:ro
      - ./certs:/certs
    environment:
      METRICS_TOKEN: "${METRICS_TOKEN}"
    restart: unless-stopped

  api:
    image: my-api:latest
    expose: ["4000"]
```

### Build your own image

```bash
git clone https://github.com/lopatnov/conduit
cd conduit
docker build -f contrib/Dockerfile -t conduit:local .
```

---

## systemd

The `contrib/conduit.service` unit file is ready for production use. It includes
security hardening (`NoNewPrivileges`, `ProtectSystem=strict`) and automatic restart.

```bash
# Install the binary
sudo cp target/release/conduit /usr/local/bin/

# Create a dedicated user
sudo useradd --system --no-create-home --shell /usr/sbin/nologin conduit

# Install config and unit
sudo mkdir -p /etc/conduit /var/log/conduit /var/cache/conduit
sudo cp conduit.json /etc/conduit/conduit.json
sudo cp contrib/conduit.service /etc/systemd/system/

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable --now conduit

# Hot-reload config without restart
sudo systemctl reload conduit
# or:
conduit reload --admin 127.0.0.1:2019
```

Config live at `/etc/conduit/conduit.json`. Environment variables (secrets, tokens)
go in `/etc/conduit/conduit.env` — the unit file loads it automatically.

---

## Kubernetes

Conduit is not a Kubernetes-native ingress controller, but it fits well in several
k8s deployment patterns.

### As a sidecar proxy

Run Conduit alongside your application container in the same Pod. Conduit handles
TLS termination, compression, rate limiting, and serves the frontend bundle while
your app only deals with API logic.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: myapp
spec:
  template:
    spec:
      containers:
        - name: api
          image: myapp:latest
          ports:
            - containerPort: 4000

        - name: conduit
          image: ghcr.io/lopatnov/conduit:latest
          ports:
            - containerPort: 8080
          volumeMounts:
            - name: conduit-config
              mountPath: /etc/conduit
            - name: static-files
              mountPath: /dist
      volumes:
        - name: conduit-config
          configMap:
            name: conduit-config
        - name: static-files
          emptyDir: {}   # populated by an init container
```

### As a DaemonSet (node-level proxy)

Deploy one Conduit instance per node to serve as a lightweight ingress or proxy
for node-local traffic.

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
    spec:
      hostNetwork: true
      containers:
        - name: conduit
          image: ghcr.io/lopatnov/conduit:latest
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

### ConfigMap for configuration

Store the Conduit config as a Kubernetes ConfigMap so it can be updated without
rebuilding the image.

```bash
kubectl create configmap conduit-config \
  --from-file=conduit.json=./conduit.json
```

To hot-reload after updating the ConfigMap, send a reload request through the Admin
API (requires a ClusterIP Service on port 2019):

```bash
conduit reload --admin <pod-ip>:2019
```

### Compared to nginx-ingress

| Feature | nginx-ingress | Conduit |
|---|---|---|
| TLS termination | ✅ | ✅ |
| Path-based routing | ✅ | ✅ |
| Load balancing | ✅ | ✅ (7 strategies) |
| Rate limiting | via annotations | ✅ native |
| Auth (Basic/API key) | via annotations | ✅ native |
| Prometheus metrics | via exporter | ✅ native |
| Config format | YAML annotations | JSON / YAML |
| k8s-native CRDs | ✅ | ❌ |
| Automatic cert discovery | ✅ cert-manager | ✅ ACME built-in |

Conduit is a good fit when you want a **simple, self-contained proxy** without the
complexity of Kubernetes Ingress controllers. For large clusters with many services
and teams, a dedicated ingress controller (nginx, Traefik, Envoy) scales better.

---

## Environment Variables

| Variable        | Default          | Description                                      |
| --------------- | ---------------- | ------------------------------------------------ |
| `RUST_LOG`      | `warn`           | Log level: `error` `warn` `info` `debug` `trace` |
| `CONDUIT_ADMIN` | `127.0.0.1:2019` | Admin API address for `conduit reload/status`    |

Secrets in the config file can reference environment variables:

```json
{ "basicAuth": { "users": { "admin": "$ADMIN_PASSWORD" } } }
```

Conduit substitutes `$VAR` at startup. Use a `.env` file (loaded by systemd or
your container orchestrator) or pass variables directly to the process.
