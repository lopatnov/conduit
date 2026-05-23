# @lopatnov/conduit

[![npm](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/lopatnov/conduit/blob/main/LICENSE)

**High-performance reverse proxy and static file server** — powered by [Cloudflare Pingora](https://github.com/cloudflare/pingora). Runs as a native Rust binary, distributed via npm for convenience.

- **150k+ req/s** static files · **80k+ req/s** proxy passthrough
- **Single binary** — no Node.js runtime needed after install
- **One JSON file** describes your entire server
- **Hot-reload** in dev · **Auto-TLS** (Let's Encrypt) in production
- Drop-in upgrade for `express-reverse-proxy` with 10–20× more throughput

## Installation

```bash
# Use without installing
npx @lopatnov/conduit

# Or install globally
npm install -g @lopatnov/conduit
```

The `postinstall` script automatically downloads the correct pre-built binary for your
platform from [GitHub Releases](https://github.com/lopatnov/conduit/releases).

To skip the download (e.g., you built from source and placed the binary yourself):

```bash
CONDUIT_SKIP_DOWNLOAD=1 npm install @lopatnov/conduit
```

## Quick Start

```bash
# Interactive setup wizard
conduit init

# Start the server
conduit

# Validate config without starting
conduit validate
```

Minimal `conduit.json`:

```json
{
  "port": 3000,
  "static": "./dist",
  "proxy": { "/api": "http://localhost:4000" }
}
```

## CLI

```
conduit                         start server (reads conduit.json)
conduit -c <file>               use a specific config file
conduit init                    interactive wizard
conduit validate                validate config (exit 0 = OK)
conduit probe                   HEAD to every upstream
conduit fmt [--write]           pretty-print config
conduit reload                  hot-reload config (no restart)
conduit status                  server status
conduit upstreams               upstream health and latency
conduit shutdown                graceful shutdown
conduit --version
```

## Supported Platforms

| Platform | Architecture | Supported |
|---|---|---|
| Linux | x86-64 | ✅ |
| Linux | ARM64 | ✅ |
| macOS | x86-64 (Intel) | ✅ |
| macOS | ARM64 (Apple Silicon) | ✅ |
| Windows | x86-64 | ✅ |

## Alternatives

If the binary download fails or your platform is not supported, install from source:

```bash
cargo install conduit-proxy
```

Or download a pre-built binary directly from
[GitHub Releases](https://github.com/lopatnov/conduit/releases).

## Documentation

Full documentation: <https://github.com/lopatnov/conduit>

## License

[Apache 2.0](https://github.com/lopatnov/conduit/blob/main/LICENSE)
