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

Conduit ships Pingora **0.8** which contains all three fixes above.
The custom `ConduitCacheKey` (host + scheme + path + query) is required by design —
Pingora 0.8 removed the default cache key implementation to force explicit opt-in.

## Known Unfixable Transitive Vulnerabilities

These advisories affect transitive dependencies that Conduit cannot upgrade without waiting for
an upstream project to update first. Each entry explains why it cannot be fixed and what the
actual risk is.

### RUSTSEC-2024-0437 — protobuf 2.28.0: Uncontrolled Recursion / Crash

| Field | Value |
|---|---|
| Advisory | [RUSTSEC-2024-0437](https://rustsec.org/advisories/RUSTSEC-2024-0437) |
| Affected crate | `protobuf 2.28.0` |
| Fix requires | `protobuf ≥ 3.7.2` |
| Status | **Acknowledged — cannot fix without upstream change** |
| Tracked in | `.cargo/audit.toml` (`ignore = ["RUSTSEC-2024-0437"]`) |

**Root cause chain:**

```text
conduit → pingora-core 0.8.0 → prometheus 0.13.4 → protobuf ^2 (uses 2.x API)
```

`prometheus 0.13.x` uses the `protobuf 2.x` API exclusively and is incompatible with
`protobuf 3.x`. Upgrading `protobuf` to ≥ 3.7.2 would require Pingora to upgrade their
`prometheus` dependency to `0.14.x`, which is an upstream decision.

**Why Conduit is not at risk:**

The vulnerability allows a crash via uncontrolled recursion when **parsing crafted protobuf
binary data**. Conduit never parses protobuf data from untrusted sources — `prometheus` is
used exclusively as a write-only metrics output library. Prometheus text-format scraping
does not involve protobuf parsing.

**Our own direct `prometheus 0.14` dependency already uses `protobuf 3.7.2`** (not vulnerable).
The vulnerable `protobuf 2.28.0` is only reachable through Pingora's own metrics code path.

**Blocked by:** Pingora upstream upgrading `prometheus 0.13.4` → `0.14.x`.

## Security Design Decisions

- **Admin API binds to loopback only** (`127.0.0.1:2019`) — never exposed to the network
- **Upload server binds to `127.0.0.1:0`** — OS-assigned port, not configurable
- **No `native-tls`** — Conduit uses rustls exclusively (no OpenSSL, no SChannel)
- **TLS ciphers** use rustls string format, not OpenSSL names — no ambiguity
- **`$VAR` interpolation** is limited to config values — not keys, not config structure
- **IP filter** is applied before auth and rate limiting
- **Health and metrics endpoints** bypass auth by design — protect with `ipFilter` if needed
