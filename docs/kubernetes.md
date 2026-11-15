# Running Conduit from Kubernetes resources

With the `kubernetes` feature Conduit reads its sites from `ConduitSite` objects instead of a config file. Each object is
one virtual site; Conduit watches the namespace, builds one config from every object, validates it and swaps it in
without a restart.

<p align="center">
  <img src="img/kubernetes.svg" alt="You apply a ConduitSite resource with kubectl. The Kubernetes API stores it next to the other ConduitSite resources. Conduit watches the namespace, builds one configuration from every resource, checks it and swaps it in without a restart; an invalid update is rejected. Requests go to the services each site names." width="900">
</p>

## Table of Contents

- [What you need](#what-you-need)
- [Install](#install)
- [Your first site](#your-first-site)
- [What a `ConduitSite` may contain](#what-a-conduitsite-may-contain)
- [What happens on every change](#what-happens-on-every-change)
- [What does not apply live](#what-does-not-apply-live)
- [Who may write `ConduitSite` objects](#who-may-write-conduitsite-objects)
- [Troubleshooting](#troubleshooting)

## What you need

- The **full** image, `ghcr.io/lopatnov/conduit:latest` (or a build with `--features kubernetes`). The other image
  variants do not contain the Kubernetes provider; see [deployment.md](deployment.md#docker).
- `kubectl` access to a cluster that may create a `CustomResourceDefinition`.
- A service account for the Conduit pod that may `get`, `list` and `watch` `conduitsites`.

The pod is started with `--kubernetes-namespace <namespace>` (`'*'` for every namespace) and no config file. Conduit
connects with the pod's service account, or with `KUBECONFIG` outside a cluster.

## Install

**1. The `ConduitSite` definition, once per cluster:**

```bash
kubectl apply -f contrib/k8s/conduitsite-crd.yaml
```

**2. A service account that may read the objects.** One namespace needs a `Role`; `--kubernetes-namespace '*'` needs a
`ClusterRole` and a `ClusterRoleBinding` with the same rules.

<!-- docs-check: skip (Kubernetes RBAC manifests, not a Conduit config file) -->
```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: conduit
  namespace: default
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: conduit
  namespace: default
rules:
  - apiGroups: ["conduit.io"]
    resources: ["conduitsites"]
    verbs: ["get", "list", "watch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: conduit
  namespace: default
subjects:
  - kind: ServiceAccount
    name: conduit
    namespace: default
roleRef:
  kind: Role
  name: conduit
  apiGroup: rbac.authorization.k8s.io
```

Conduit only reads: it needs no `update` or `patch`, and it does not write the `status` of an object (the `READY`
column of `kubectl get conduitsites` stays empty).

**3. A Deployment and a Service.** A site listens on the `port` of its spec; the Service forwards to the same port.

<!-- docs-check: skip (Kubernetes Deployment and Service manifests, not a Conduit config file) -->
```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: conduit
  namespace: default
spec:
  replicas: 2
  selector:
    matchLabels: { app: conduit }
  template:
    metadata:
      labels: { app: conduit }
    spec:
      serviceAccountName: conduit
      containers:
        - name: conduit
          image: ghcr.io/lopatnov/conduit:latest
          args: ["--kubernetes-namespace", "default"]
          securityContext:
            runAsNonRoot: true
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities: { drop: [ALL] }
          ports:
            - { name: http, containerPort: 8080 }
          readinessProbe:
            httpGet: { path: /__health__, port: http }
---
apiVersion: v1
kind: Service
metadata:
  name: conduit
  namespace: default
spec:
  selector: { app: conduit }
  ports:
    - { name: http, port: 80, targetPort: http }
```

The readiness probe asks for `/__health__`, which exists only on a site that sets `healthCheck: true`: give the site on
port 8080 that setting, as the example below does. The image runs as an unprivileged user, so a site listens on a port
above 1024 unless the pod is given the right to bind lower ones.

The Admin API listens on `127.0.0.1:2019` inside the pod in this mode; its bind address cannot be set from a
`ConduitSite`.

## Your first site

<!-- docs-check: skip (a ConduitSite manifest, not a Conduit config file) -->
```yaml
apiVersion: conduit.io/v1
kind: ConduitSite
metadata:
  name: my-app
  namespace: default
spec:
  port: 8080
  host: app.example.com
  healthCheck: true
  proxy:
    /api:
      targets: [http://api-svc:4000]
      healthCheck: { path: /health }
  rateLimit: { windowSecs: 60, limit: 500 }
  securityHeaders: true
```

```bash
kubectl apply -f my-app.yaml
kubectl get conduitsites            # PORT, HOST and AGE of every site
kubectl logs deploy/conduit | grep ConduitSite
```

Within a few seconds the log shows `config updated from ConduitSite CRDs` and then `live config update received`, and
the site answers.

A `spec` holds the same fields as one entry of `sites:` in a config file. To check a spec before it goes to the cluster,
put it in a file and run `conduit validate`:

```yaml
sites:
  - port: 8080
    host: app.example.com
    healthCheck: true
    proxy:
      /api:
        targets: [http://api-svc:4000]
        healthCheck: { path: /health }
    rateLimit: { windowSecs: 60, limit: 500 }
    securityHeaders: true
```

```bash
conduit validate -c sites.yaml
```

`conduit validate` runs the checks every config gets. It does not apply the rule of the next section: a path on the host
is accepted in a file and refused in a `ConduitSite`.

### More examples

An API that needs a JWT (the keys are fetched from the JWKS URL; nothing secret is in the object):

<!-- docs-check: skip (a ConduitSite manifest, not a Conduit config file) -->
```yaml
apiVersion: conduit.io/v1
kind: ConduitSite
metadata:
  name: my-api
  namespace: default
spec:
  port: 8081
  host: api.example.com
  jwtAuth:
    jwksUrl: https://auth.example.com/.well-known/jwks.json
  proxy:
    /v1: http://api-svc:4000
  healthCheck: true
```

Two upstreams with the least busy one chosen, passive ejection of a failing one, and the upstream's 5xx bodies hidden:

<!-- docs-check: skip (a ConduitSite manifest, not a Conduit config file) -->
```yaml
apiVersion: conduit.io/v1
kind: ConduitSite
metadata:
  name: orders
  namespace: default
spec:
  port: 8082
  host: orders.example.com
  proxy:
    /:
      targets: [http://orders-a:4000, http://orders-b:4000]
      strategy: least-conn
  outlierDetection: { consecutive5xx: 5, baseEjectionTimeSecs: 30 }
  maskErrors: true
```

A raw TCP site (a database, needs the `tcp` feature of the full image). It has no `host`, as TCP has no `Host` header:

<!-- docs-check: skip (a ConduitSite manifest, not a Conduit config file) -->
```yaml
apiVersion: conduit.io/v1
kind: ConduitSite
metadata:
  name: postgres
  namespace: default
spec:
  port: 5432
  tcp:
    targets: ["postgres.default.svc:5432"]
```

## What a `ConduitSite` may contain

Every field of a site of the [configuration reference](configuration.md): `proxy` and `routes`, `tls`, `jwtAuth`,
`consumers`, `forwardAuth`, `rateLimit`, `ipFilter`, `limits`, `securityHeaders`, `cors`, `compression`, `redirects`,
`headers`, `fallback`, `tcp` and the rest. A key that is not a site field is reported in the log as a warning and has no
effect, as in a config file; check the spelling of a field that seems to do nothing.

Three differences from a file:

- **No host paths.** A spec that sets one is refused: `static`, `upload`, `middleware`, `tls.cert`, `tls.key`, `tls.ca`,
  `tls.clientAuth`, `tls.acme.storage`, `logging.file`, `fallback.file` and `fallback.byAccept.*.file`, also inside
  `routes`. Serve static content from an upstream, get certificates from `tls.acme`, and use a file config for the rest.
  `tls.acme` keeps its certificates in `./certs` of the working directory, which is `/certs` in the image: mount a
  writable volume there, because the root filesystem of the example above is read-only.
- **No `$VAR` expansion.** A config file expands `$VAR` and `${VAR}` from the environment; a `ConduitSite` is taken as
  written, so `$PASSWORD` is the literal text `$PASSWORD`. Whatever a spec holds is readable by anyone who may `get`
  the object, so do not put a secret in one that those people should not see. Prefer `jwtAuth` (a public JWKS URL) or
  `forwardAuth` over `basicAuth` and `apiKey`, whose credentials are plain text in the spec.
- **No `global`.** Workers, the Admin API and the shutdown period belong to the pod, not to a site. Conduit supplies a
  config whose Admin API is on loopback with no token.

## What happens on every change

Any `ConduitSite` that is added, changed or deleted makes Conduit list all objects in the namespace again and build the
whole config from them. The result is checked like a config file and swapped in only if it passes.

| Step                         | Failure                                                                                  | Result                                                                                |
| ---------------------------- | ---------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| Read the spec of every site  | one spec is refused (a host path, a field of the wrong type)                             | `failed to rebuild config from ConduitSite CRDs — keeping current config`             |
| Validate the whole config    | a hard error: a duplicate `host` and `port`, a bad TLS setting, ...                       | `config error at <path>: <message>` and `live config update rejected`                 |
| Compare with the running one | a change the running listeners cannot follow (see below)                                 | `cold field changed — restart required` and `live config update rejected`             |
| Swap                         | none                                                                                     | the new config is in use, without a restart                                           |

A rejected update leaves the **whole** previous config in place: one invalid `ConduitSite` holds back the change of every
other site until it is fixed or deleted. At start the same failure stops the process (the pod restarts and then waits
in `CrashLoopBackOff`), because there is no previous config to keep. Warnings, such as a certificate close to expiry,
are logged and do not stop an update.

## What does not apply live

These need a new pod, for example `kubectl rollout restart deployment/conduit`:

- **A new or a removed port.** The listeners are bound at start. Adding a site on a port no site uses, or deleting the
  last site of a port, is rejected as `listeners[<port>].port`. Another site on a port that is already bound is live.
- **`tls.cert` and `tls.key`** of a port (a `ConduitSite` cannot set them anyway) and every field of **`tls.acme`**
  (`email`, `directory`, `challenge`, `profile`).
- `global.workers`, `global.shutdownTimeoutSecs` and the Admin API bind: not settable from a `ConduitSite`.

Everything else is live: proxy targets, routes, auth, rate limits, headers, redirects, `limits`, and so on. A runtime
change made through the Admin API (`conduit upstreams add`, a dynamic `ip-deny`) is kept in memory only and is
replaced by the next update.

## Who may write `ConduitSite` objects

A `ConduitSite` decides which requests Conduit accepts and where it sends them, so write access to the object is
access to that traffic. The host-path rule above stops a writer from reading, writing or serving files of the Conduit
pod; it does not stop them from routing a `host` and `port` to any URL the pod can reach, including services of other
namespaces. In practice:

- Give `create` and `update` on `conduitsites` only to the people who may configure that traffic, with a `Role` in the
  namespace you watch. Watching `'*'` makes every namespace's writers able to change the one shared config.
- Two sites with the same `host` and `port` are a validation error for the **whole** config (the table above): in
  `'*'` mode a mistake, or a deliberate collision, in one namespace holds back the updates of all the others. If tenants
  are not to be able to affect each other, run one Conduit per namespace.
- A site that uses `tls.acme` must have a `host` that is a plain DNS name; it becomes a file name in `./certs`.

## Troubleshooting

| You see                                                              | Why                                                                                                   |
| -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| The pod exits at start with `kubernetes provider error: ...`         | The API call failed: no RBAC for `list`/`watch` `conduitsites`, the CRD is not installed, or the wrong namespace |
| The pod exits with `host file paths cannot be set from a ConduitSite` | A spec names a host path; the message lists them. Remove them, or use a file config                  |
| `kubectl apply` works, the site never changes                        | Look for `live config update rejected` and `keeping current config` in `kubectl logs`: another site, or this one, is invalid |
| A field of the spec has no effect                                    | `unknown key` warning in the log: the field name is misspelled (field names are camelCase, as in a config file) |
| `cold field changed — restart required`                              | See [what does not apply live](#what-does-not-apply-live)                                              |
| `kubectl get conduitsites` shows an empty `READY`                    | Conduit does not write the status; `READY` is not used                                                |
| `ConduitSite watch error` in the log                                 | The watch on the API server failed; Conduit keeps the config it has                                   |

The effective config is not printed. To see what Conduit built, look at the log lines above and at the objects
(`kubectl get conduitsites -o yaml`): the config is exactly the list of their specs, in the order the API returns them.
