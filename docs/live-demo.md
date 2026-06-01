# Live Demo

The repository includes a self-contained demo with **two virtual sites running
from a single Conduit process**, a round-robin load balancer across two API
backends, proxy caching, Basic Auth, and more.

## Running the demo

```bash
# Terminal 1 — start two mock API instances (ports 4000 and 4001)
node demo/api/server.js

# Terminal 2 — start Conduit with the demo config
conduit -c demo/conduit.json
```

**VS Code users:** run the _"Demo: Start (Conduit + API)"_ task
(`Terminal → Run Task…`) to launch both processes at once.

## What's running

| URL | Description |
| --- | ----------- |
| [http://localhost:8080](http://localhost:8080) | Public app — proxy, cache, compression, rate limiting |
| [http://localhost:8081](http://localhost:8081) | Admin panel — protected with Basic Auth (`admin / demo1234`) |

## What the demo shows

- **Two virtual sites** from one binary — dispatched by port
- **Round-robin load balancing** across `api:4000` and `api:4001`
- **Proxy cache** — second request for the same resource returns from cache
- **Basic Auth** — the admin panel rejects unauthenticated requests
- **Rate limiting** — hit the public app quickly to see `429 Too Many Requests`
- **Compression** — static assets served with Brotli / gzip
- **Health endpoint** — `GET http://localhost:8080/__health__`
- **Metrics endpoint** — `GET http://localhost:8080/__metrics__` (Prometheus format)

## Demo config walkthrough

The demo config lives in [`demo/conduit.json`](../demo/conduit.json).
Key sections:

```json
{
  "global": { "admin": { "bind": "127.0.0.1:2019" } },
  "sites": [
    {
      "port": 8080,
      "proxy": {
        "/api": {
          "targets": ["http://localhost:4000", "http://localhost:4001"],
          "strategy": "round-robin",
          "cache": { "store": "memory", "ttlSecs": 30 }
        }
      },
      "rateLimit": { "windowSecs": 10, "limit": 20 },
      "healthCheck": true,
      "metrics": { "path": "/__metrics__" }
    },
    {
      "port": 8081,
      "basicAuth": { "users": { "admin": "demo1234" } },
      "proxy": { "/": "http://localhost:4000" }
    }
  ]
}
```

See [`demo/README.md`](../demo/README.md) for the full walkthrough.
