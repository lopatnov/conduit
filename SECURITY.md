# Security Policy

## Supported Versions

| Version | Supported |
|---|---|
| latest (`main`) | ✅ |
| older releases | ❌ |

We only backport security fixes to the latest release. Always upgrade to the latest version.

## Reporting a Vulnerability

**Please do not file a public GitHub issue for security vulnerabilities.**

Email: **lopatniov@gmail.com**

Include in your report:

1. Description of the vulnerability
2. Steps to reproduce (minimal `conduit.json` + request example)
3. Affected versions
4. Potential impact
5. (Optional) Suggested fix or patch

You will receive a confirmation within **48 hours** and a resolution timeline within **7 days**.

## Disclosure Policy

- Security issues are fixed in a private fork and released as a patch version
- A GitHub Security Advisory is published at the same time as the fix
- Credit is given to the reporter unless they prefer to remain anonymous
- Coordinated disclosure window: **90 days** from initial report

## Known Issues Fixed

| CVE | Fixed in | Description |
|---|---|---|
| CVE-2026-2833 | Pingora 0.8 | HTTP/2 header handling |
| CVE-2026-2835 | Pingora 0.8 | Connection pool race |
| CVE-2026-2836 | Conduit (all) | Custom cache key required (Pingora default removed) |

Conduit ships Pingora **0.9**, which carries all three fixes above (they landed in 0.8).
The custom `ConduitCacheKey` (host + scheme + path + query) is required by design —
Pingora 0.8 removed the default cache key implementation to force explicit opt-in, and 0.9
also dropped the separate `namespace` argument of `CacheKey`, so Conduit frames the host into
the key itself (the host, a NUL byte, then `scheme:path?query`).

## Known Unfixable Transitive Vulnerabilities

None at present. Advisories that Conduit cannot fix itself because an upstream project has to
update first are listed here, each with why it cannot be fixed and what the actual risk is.

### Resolved: RUSTSEC-2024-0437 — protobuf 2.28.0: Uncontrolled Recursion / Crash

Resolved by the Pingora 0.9 upgrade (PR #450). `pingora-core 0.8` depended on
`prometheus 0.13.4`, which requires `protobuf ^2`, so the vulnerable `protobuf 2.28.0` was in
the dependency tree. It was only reachable through Pingora's own metrics code, never through
untrusted input. Pingora 0.9 no longer depends on `prometheus` from `pingora-core`; the only
`protobuf` left is `3.7.2` (through Conduit's own `prometheus 0.14`), which is not affected.
The matching `cargo-audit` ignore in `.cargo/audit.toml` has been removed.

## Security Design Decisions

- **Admin API binds to loopback only** (`127.0.0.1:2019`) — never exposed to the network
- **Upload server binds to `127.0.0.1:0`** — OS-assigned port, not configurable
- **No `native-tls`** — Conduit uses rustls exclusively (no OpenSSL, no SChannel)
- **TLS ciphers** use rustls string format, not OpenSSL names — no ambiguity
- **`$VAR` interpolation** is limited to config values — not keys, not config structure
- **IP filter** is applied before auth and rate limiting
- **Health and metrics endpoints** bypass auth by design — protect with `ipFilter` if needed

## Redis TLS (`rediss://`)

`rateLimit.store` accepts both `redis://` (plaintext) and `rediss://` (TLS) URLs.

Use `rediss://` when your Redis deployment requires in-transit encryption, e.g.:
- **AWS ElastiCache** with TLS enabled
- **Azure Cache for Redis** (TLS is on by default on port 6380)
- **Upstash** and other hosted Redis providers

```jsonc
"rateLimit": {
  "windowSecs": 60,
  "limit": 100,
  "store": "rediss://your-redis-host:6380"
}
```

If your Redis requires TLS but only exposes a non-`rediss://` endpoint, use a
TLS-terminating proxy (stunnel, nginx stream) in front of it and connect via `redis://`.
