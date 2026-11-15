# Technologies and their licences

Conduit is written in Rust and stands on the projects below. All of them are open source under licences that are
compatible with Conduit's own [Apache-2.0](../LICENSE) licence, and the ones listed here are permissive. Across the whole
build, one crate carries a weak, file-level copyleft licence: `smartstring` (MPL-2.0, used unmodified, through Rhai); its
terms are in the generated notices file below.

This page lists the technologies that shape what Conduit does. It is written by hand. The complete list is generated
from `Cargo.lock`: every crate that can end up in a Conduit binary, its licence, and the full licence and notice texts
are in [`THIRD-PARTY-LICENSES.txt`](../THIRD-PARTY-LICENSES.txt), which is also inside every release archive, the
Debian package, the npm package (gzipped) and the Docker image (`/licenses/`), and is attached to every GitHub
release. CI checks both: a
dependency outside the allow-list in [`ci/license-policy.json`](../ci/license-policy.json) fails the build, and so does
a row below whose licence differs from the one `Cargo.lock` says (`scripts/third_party_licenses.py --check`).

Apache-2.0 section 4(d) asks a redistributor to carry the attribution notices of a dependency's `NOTICE` file. At the
time of writing none of the dependencies ships one (`scripts/third_party_licenses.py` looks for them in every package and
says so at the top of `THIRD-PARTY-LICENSES.txt`); if one does, its text is reproduced in that file, which travels with
every artifact. Conduit itself has no `NOTICE` file: the licence does not require one, and a project that has one puts
the duty to carry it on everyone who redistributes it.

A licence written as `A OR B` lets the user of the crate pick one of the two; `A AND B` means both apply to different
parts of it. The identifiers are [SPDX](https://spdx.org/licenses/) names.

## Proxy engine and runtime

| Crate | What Conduit uses it for | Licence |
|---|---|---|
| `pingora-core` | The proxy engine from Cloudflare: connection handling, the request phases, upstream pools, load balancing, caching (`pingora-proxy`, `pingora-cache` and the other `pingora-*` crates carry the same licence) | Apache-2.0 |
| `tokio` | Asynchronous runtime for the Admin API, health checks, ACME, mirroring and everything outside the Pingora worker threads | MIT |
| `mimalloc` | The memory allocator of the `conduit` binary (feature `mimalloc`, on by default). Its C sources are Microsoft's mimalloc, under MIT; the notice is in `THIRD-PARTY-LICENSES.txt` | MIT |
| `hyper` | HTTP implementation under the Admin API, the upload handler and the HTTP clients | MIT |
| `h2` | HTTP/2 | MIT |
| `axum` | The Admin API and the upload listener | MIT |
| `tower` | Service layers around the Axum routers | MIT |
| `reqwest` | The HTTP client for traffic mirroring, JWKS refresh and ForwardAuth | MIT OR Apache-2.0 |

## TLS and certificates

| Crate | What Conduit uses it for | Licence |
|---|---|---|
| `rustls` | TLS 1.2 and 1.3 for listeners and upstream connections | Apache-2.0 OR ISC OR MIT |
| `aws-lc-rs` | The cryptography library under rustls | ISC AND (Apache-2.0 OR ISC) |
| `ring` | Cryptographic primitives (hashing, signatures) | Apache-2.0 AND ISC |
| `rustls-webpki` | Certificate chain validation | ISC |
| `webpki-root-certs` | The bundled Mozilla root certificates, for upstreams that are verified against the public roots (the licence text of this data, CDLA-Permissive-2.0, is in `THIRD-PARTY-LICENSES.txt`) | CDLA-Permissive-2.0 |
| `instant-acme` | Automatic certificates from an ACME CA such as Let's Encrypt | Apache-2.0 |
| `rcgen` | Self-signed certificates and certificate signing requests | MIT OR Apache-2.0 |

## Authentication

| Crate | What Conduit uses it for | Licence |
|---|---|---|
| `jsonwebtoken` | JWT validation, including JWKS keys | MIT |
| `hmac` | Signed sticky-session cookies | MIT OR Apache-2.0 |
| `sha2` | Hashing (consistent hash, checksums, API-key digests) | MIT OR Apache-2.0 |

## Configuration and data

| Crate | What Conduit uses it for | Licence |
|---|---|---|
| `serde` | Reading the configuration | MIT OR Apache-2.0 |
| `serde_json` | JSON configuration, the Admin API and the log output | MIT OR Apache-2.0 |
| `serde_yaml` | YAML configuration | MIT OR Apache-2.0 |
| `serde_path_to_error` | Error messages that name the exact configuration field | MIT OR Apache-2.0 |
| `schemars` | The JSON Schema of the configuration | MIT |
| `indexmap` | Maps that keep the order of the configuration (routes, sites) | Apache-2.0 OR MIT |
| `arc-swap` | Hot reload without locks on the request path | MIT OR Apache-2.0 |
| `dashmap` | Concurrent tables (rate limits, counters) | MIT |
| `clap` | The command line | MIT OR Apache-2.0 |
| `notify` | Watching the configuration and static files for changes | CC0-1.0 |
| `kube` | The Kubernetes configuration provider | Apache-2.0 |

## Compression

| Crate | What Conduit uses it for | Licence |
|---|---|---|
| `brotli` | Brotli responses | BSD-3-Clause AND MIT |
| `zstd` | Zstandard responses | BSD-3-Clause |
| `flate2` | gzip and deflate | MIT OR Apache-2.0 |
| `zlib-rs` | The zlib implementation under flate2 | Zlib |

## Scripting and extensions

| Crate | What Conduit uses it for | Licence |
|---|---|---|
| `rhai` | Middleware scripts (`type: "script"`) | MIT OR Apache-2.0 |
| `wasmtime` | WebAssembly middleware (`type: "wasm"`) | Apache-2.0 WITH LLVM-exception |

## Observability and shared state

| Crate | What Conduit uses it for | Licence |
|---|---|---|
| `tracing` | Structured logging inside Conduit | MIT |
| `prometheus` | The metrics format and registry | Apache-2.0 |
| `opentelemetry` | OTLP trace export | Apache-2.0 |
| `redis` | The shared rate-limit backend | BSD-3-Clause |

The Rust standard library and the compiler are dual-licensed under MIT OR Apache-2.0.
