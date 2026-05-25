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

## Security Design Decisions

- **Admin API binds to loopback only** (`127.0.0.1:2019`) — never exposed to the network
- **Upload server binds to `127.0.0.1:0`** — OS-assigned port, not configurable
- **No `native-tls`** — Conduit uses rustls exclusively (no OpenSSL, no SChannel)
- **TLS ciphers** use rustls string format, not OpenSSL names — no ambiguity
- **`$VAR` interpolation** is limited to config values — not keys, not config structure
- **IP filter** is applied before auth and rate limiting
- **Health and metrics endpoints** bypass auth by design — protect with `ipFilter` if needed
