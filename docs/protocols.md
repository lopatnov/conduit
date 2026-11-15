# Proxying protocols: SSE, WebSocket, gRPC, HLS, MQTT, PostgreSQL and more

Conduit has two ways to carry a protocol, and the first question for any protocol is which one it needs.

| Mode | What Conduit does | Pick it when | Configured as |
| ---- | ----------------- | ------------ | ------------- |
| **HTTP** | Parses the request, runs the [filter chain](../README.md) (IP filter, rate limit, auth, scripts…), routes by host and path, load-balances, retries, caches. | The protocol is carried by HTTP: SSE, WebSocket, gRPC, GraphQL, HLS, WebRTC signalling. | `proxy:` or `static:` on a site |
| **TCP** | Relays bytes in both directions. Never looks inside; none of the HTTP features apply. Needs `--features tcp`. | Anything else on a TCP port: databases, brokers, mail, FTP, SSH, RTMP. | `tcp:` on a site, one port per site |

| Protocol | Mode | What you set | Tested by |
| -------- | ---- | ------------ | --------- |
| [SSE](#server-sent-events-sse) | HTTP | `timeout.readMs` for silent streams | `scripts/e2e/streaming.sh` |
| [WebSocket](#websocket) | HTTP | `websocket: true`, `timeout.readMs` | `scripts/e2e/streaming.sh` |
| [gRPC](#grpc) | HTTP | HTTP/2 on both sides, `http2: "h2c"` for a plaintext upstream | `scripts/e2e/grpc.sh` |
| [GraphQL](#graphql) | HTTP | nothing special; subscriptions use WebSocket or SSE | covered by the WebSocket and SSE checks |
| [WebDAV](#webdav) | HTTP | `limits.maxBodyBytes` for uploads; keep the upstream at the same path | `scripts/e2e/webdav.sh` |
| [HLS / DASH](#hls-and-dash) | HTTP (`static:` or `proxy:`) | `staticOptions.maxAge` | by hand (see below) |
| [WebRTC](#webrtc) | HTTP for signalling; media is UDP and cannot be proxied | — | signalling is WebSocket |
| [RTMP](#rtmp) | TCP | one site on port 1935 | `scripts/e2e/protocols.sh` |
| [MQTT](#mqtt) | TCP (or WebSocket) | one site on 1883 / 8883 | `scripts/e2e/protocols.sh` |
| [PostgreSQL](#postgresql) | TCP | one site on 5432 | `scripts/e2e/protocols.sh` |
| [Redis](#redis) | TCP | one site on 6379 | `scripts/e2e/protocols.sh` |
| [SMTP](#smtp) | TCP | one site on 25 / 587 | `scripts/e2e/protocols.sh` |
| [FTP](#ftp) | TCP | the control port **and every passive data port** | `scripts/e2e/protocols.sh` |
| [SFTP / SSH](#sftp-and-ssh) | TCP | one site on 22 | `scripts/e2e/protocols.sh` |

The e2e scenarios run Conduit in a Docker network against small servers (for the wire protocols, servers that answer the
opening exchange of the real protocol, not a full database). `scripts/e2e/run.sh` runs them.

## What a TCP site does and does not do

```yaml
sites:
  - port: 5432
    tcp:
      targets: ["db-primary.internal:5432"]
      connectTimeoutMs: 5000
```

Several targets (`strategy: round-robin` or `random`) are for interchangeable backends, such as a pool of read replicas on
a site of their own. Do not list a primary and a replica together: Conduit does not know their roles.

- It relays bytes both ways and passes a half-close on, so a client that finishes sending and waits for the answer works.
- It does **not** terminate TLS. A TLS client talks to the upstream through Conduit and verifies the **upstream's**
  certificate (checked in `protocols.sh` with `openssl s_server`). That is also how `STARTTLS`-style upgrades and
  PostgreSQL's `SSLRequest` pass through.
- It adds nothing to the stream: the upstream sees Conduit's address as the client, because there is no PROXY protocol
  support. Anything that decides by client address on the backend (`pg_hba.conf`, mail relay rules, fail2ban) sees Conduit.
- None of the HTTP features apply: no IP filter, rate limit, authentication or metrics per route. Conduit refuses a
  configuration that puts `ipFilter`, `rateLimit`, `basicAuth`, `apiKey`, `jwtAuth`, `forwardAuth`, `consumers`, `limits` or `middleware` on a `tcp`
  site (it would be silently ignored and look like protection). Put access control in front of the port (a firewall or
  network policy) or on the backend.
- There is no idle timeout and Conduit sets no limit on the number of connections: a connection lives until one side closes it
  (the operating system's limits on open files still apply).
- No UDP. Port ranges are not supported: one `sites` entry is one port.
- Several upstreams are load-balanced per **connection**, not per message, and a target that fails to connect is passed
  over for a few seconds (see [TCP Proxy](configuration.md#tcp-proxy)). A connection that is already relayed is never
  moved. For a database that means Conduit balances connections, not queries; it knows nothing of primary or replica roles.

## Server-Sent Events (SSE)

An event stream is a long HTTP response. No special setting is needed; the route is an ordinary proxy route:

```yaml
sites:
  - port: 443
    host: app.example.com
    tls: {cert: ./cert.pem, key: ./key.pem}
    proxy:
      "/events":
        targets: ["http://events.internal:9000"]
        timeout: {readMs: 120000}     # see the note on silence below
```

What the e2e test shows, over plain HTTP, TLS and HTTP/2:

- Events reach the client as the upstream sends them (three events 0.5 s apart arrive 0.5 s apart; the first within
  0.4 s), and the `text/event-stream` type is kept. Conduit does not compress proxied responses, so nothing holds events back.
- `Last-Event-ID` and `Accept: text/event-stream` reach the upstream, so a reconnecting `EventSource` resumes.
- **`timeout.readMs` is an idle timeout** (30 s by default): the stream is closed when the upstream sends nothing for that
  long. A stream that can stay silent longer than `readMs` must either send a comment line (`: keep-alive`) more often, or
  the route needs a larger `readMs`. A silence shorter than `readMs` is fine, a longer one ends the stream (2 s against a 4 s
  gap, and 10 s against the same gap, are both in the test).

## WebSocket

Upgrades are refused with 502 unless the route says `websocket: true`:

```yaml
    proxy:
      "/socket":
        targets: ["ws://ws.internal:9001"]
        websocket: true
        timeout: {readMs: 300000}
```

**`ws://` or `http://` in `targets`: both work, and they mean the same upstream.** A WebSocket connection starts as an
ordinary HTTP/1.1 request with `Upgrade: websocket` ([RFC 6455 §4.1](https://www.rfc-editor.org/rfc/rfc6455#section-4.1)),
and Conduit is the one that forwards that request, so the target is an address, not a protocol. `ws://` is accepted as
`http://` (port 80 by default) and `wss://` as `https://` (port 443, TLS to the upstream). Use `wss://` or `https://` when
the upstream itself speaks TLS. The route's `websocket: true` is what allows the upgrade, not the scheme of the target.
The e2e scenario uses `ws://` for its main route and `http://` for the others.

Through a plain port and a TLS port (`ws://` and `wss://`) the test passes the handshake, text messages, ping/pong,
fragmented messages, a 1 MiB binary message (hash compared) and the close handshake including the upstream's close code. The
upstream sees the request path (after `stripPrefix`), the `Origin` and `X-Forwarded-For`.

- `timeout.readMs` is an idle timeout here too: a socket in which nothing travels either way for that long is closed. A
  ping from either side counts as traffic and keeps it open, so a client that pings every 20–30 s works with the default.
- `Sec-WebSocket-*` headers pass to the upstream as sent; check the sub-protocol you need with your own client (not covered by the test).
- The upgrade is the HTTP/1.1 one (`Upgrade: websocket`). WebSocket over HTTP/2 (RFC 8441) is not covered by the test; use HTTP/1.1 for the socket.

## gRPC

gRPC is HTTP/2 with trailers. Both sides of Conduit must speak HTTP/2:

| Hop | Setting |
| --- | ------- |
| Client → Conduit over TLS | `tls:` and `http2: {}` on the site (HTTP/2 is negotiated through ALPN) |
| Client → Conduit without TLS | `http2: {h2c: true}` on the site (prior knowledge, no upgrade) |
| Conduit → upstream over TLS | `http2: true` on the route (ALPN `h2`); `upstreamTls: {verify: false}` for a self-signed upstream (this switches certificate verification off, so the connection can be intercepted: only on a network you trust, otherwise give the upstream a certificate the system trusts); the host still needs a CA bundle, see [Upstream TLS verification](configuration.md#upstream-tls-verification) |
| Conduit → upstream **without TLS** | **`http2: h2c`** on the route |

`http2: true` is not enough for a plaintext upstream: HTTP/2 is negotiated through ALPN, which does not exist without TLS,
so Conduit would speak HTTP/1.1 to it (the control case in the test shows gRPC failing that way). `http2: h2c` makes it use
HTTP/2 with prior knowledge ([RFC 9113 §3.3](https://www.rfc-editor.org/rfc/rfc9113#section-3.3)), as nginx
`grpc_pass grpc://`, Envoy and Traefik (`h2c://`) do. Over TLS `h2c` behaves like `true`. One consequence of Pingora's connection pool: an `h2c` route and an `http2: true` route that
point at the same plaintext `host:port` can share one HTTP/2 connection (requests stay separate streams), so give two routes
that need different behaviour different upstream addresses.

```yaml
sites:
  - port: 443
    host: grpc.example.com
    http2: {}
    tls: {cert: ./cert.pem, key: ./key.pem}
    proxy:
      "/": {targets: ["http://orders.internal:50051"], http2: h2c}
```

`grpc.sh` runs a unary call with trailing metadata, server streaming (messages arrive one by one), client streaming of 1 MB,
an error status with its message and trailers (`INVALID_ARGUMENT`), and a deadline (`DEADLINE_EXCEEDED`), through each
combination in the table.

- Route by service with a path prefix: gRPC paths are `/package.Service/Method`, so `"/orders.Orders/"` is a prefix.
- The idle `timeout.readMs` applies to streaming calls as well (tested for SSE and WebSocket, the same code path): raise it for calls that can be silent.
- **Active health probes speak plaintext HTTP/1.1** (a TLS upstream, `https://` or `wss://`, is probed by connecting only), which a gRPC server does not answer; with a `healthCheck` on such a route the
  upstream is marked unhealthy (observed with the Python gRPC server). Use `outlierDetection` (judges real calls) instead.
- gRPC-Web (HTTP/1.1 from a browser) is plain HTTP and needs no `h2c`, but is not part of the test.

## GraphQL

Queries and mutations are `POST` requests with a JSON body: an ordinary proxy route, all HTTP features apply (rate limit
per client, JWT, cache for `GET` persisted queries). Subscriptions travel over a WebSocket (`graphql-transport-ws`) or SSE,
so the route that serves them needs `websocket: true` or the SSE note about `readMs`.

```yaml
    proxy:
      "/graphql": {targets: ["http://api.internal:4000"], websocket: true, timeout: {readMs: 120000}}
```

## WebDAV

WebDAV (RFC 4918) is HTTP with more methods, so it is an ordinary proxy route. Nothing needs enabling: Conduit has no
list of allowed methods, and `PROPFIND`, `PROPPATCH`, `MKCOL`, `COPY`, `MOVE`, `LOCK`, `UNLOCK`, `DELETE`, `PUT` and `OPTIONS`
reach the upstream as sent.

```yaml
    limits: {maxBodyBytes: 1073741824}     # a site setting: the largest upload (1 GiB here)
    proxy:
      "/files": {targets: ["http://dav.internal:8080"], timeout: {readMs: 600000}}
```

What `webdav.sh` shows, against an upstream that echoes every request:

- The methods, the request bodies (PROPFIND and LOCK XML, a 1 MiB `PUT` compared by length and SHA-256) and the headers
  `Depth`, `Overwrite`, `Timeout`, `Lock-Token`, `If`, `Destination` and `Content-Type` arrive unchanged. The statuses
  (207, 201, 204) and the response headers `DAV`, `Allow` and `Lock-Token` come back unchanged. The client's `Host` is
  forwarded, so an upstream that compares it with the host in `Destination` (Apache `mod_dav` does) sees the same one.
- **Conduit does not rewrite URLs that the client wrote.** `Destination`, the resource tags of `If`, the `Location` of a
  `201` and the `<href>` elements of a multistatus body are in the client's namespace, and are passed on and returned as
  they are. With the default (no `stripPrefix`) the upstream is mounted at the same path as the route and all of this is
  consistent; that is the setup to start with. With `stripPrefix: true` the upstream sees paths without the prefix but
  `Destination` still has it, and the `href`s it returns do not: either the upstream is told its public prefix (Radicale, for
  one, takes it from an `X-Script-Name` header, which `requestTransform.setHeaders` can add; check your server's documentation,
  this is not covered by the test), or the route sets `rewriteDestination: true` (next point). The upstream's own absolute
  URLs (a `Location`, an `<href>` that names its internal host) are also passed on as it wrote them, so configure its public
  origin as well as its path, or a client may be sent to a host it cannot reach.
- **`rewriteDestination: true`** (opt-in, default off) applies the route's `stripPrefix` and `rewrite` rules to the paths in
  `Destination` and in the resource tags of `If` (`<URL>` outside the parentheses, RFC 4918 section 10.4), so that a
  `COPY` or `MOVE` through a stripping route names a resource the upstream knows. Only the path of a URL changes: the scheme,
  the authority, the query, the state tokens and entity tags inside the parentheses of `If` stay as they were, a URL of another
  authority than the request's `Host`, or a path outside the route's prefix, is left alone, and a request without the headers
  is untouched. Do not turn it on for an upstream that expects the public path (Radicale with `X-Script-Name`). The response
  side is not rewritten: a `Location` or `<href>` that the upstream writes in its own namespace stays as written.
  `webdav.sh` checks it on a `stripPrefix` route and on a `rewrite` route, next to the default routes that leave both
  headers alone.
- The site's `limits.maxBodyBytes` applies to uploads, so raise it for large `PUT`s. `timeout.readMs` is an idle timeout: a transfer
  that stalls for that long is closed.
- The cache stores only `GET` and `HEAD` by default, so `PROPFIND` is never served from it.

A **static site is read-only**: a file that exists is served for `GET` and `HEAD`, and every other method (`PUT`, `DELETE`,
`PROPFIND`, `MKCOL` ...) is answered **405 Method Not Allowed** with `Allow: GET, HEAD, OPTIONS`, so a WebDAV client that
tries to mount a `static:` directory is told plainly that it cannot write instead of being sent the file. Conduit does not
serve WebDAV itself; put a WebDAV server behind a `proxy:` route.

## HLS and DASH

HLS is plain HTTP: a playlist (`.m3u8`) and media segments. Two ways to serve it:

```yaml
sites:
  - port: 8080
    static: ./video
    staticOptions:
      maxAge: 1h        # the default is `no-cache`; fine for a live playlist, too cautious for old segments
```

or proxy an origin or packager with `proxy:` (add `cache:` on the route to absorb many viewers; see
[Caching](configuration.md#caching)). Checked by hand against a static folder: ranged requests answer `206` with a
`Content-Range`, `ETag` with `If-None-Match` answers `304`.

Conduit serves `.m3u8` as `application/vnd.apple.mpegurl`, `.m3u` as `audio/mpegurl`, `.ts` as `video/mp2t`, `.m4s` as
`video/iso.segment` and `.mpd` as `application/dash+xml`. (Before this guide, `.m3u8` went out as `audio/x-mpegurl` and `.ts` as
`video/vnd.dlna.mpeg-tts`, which some players and CDNs reject.)

A live playlist changes every few seconds. Do not give it a long `maxAge` or cache TTL; keep the segments and the playlist on
separate prefixes if you want to cache one and not the other.

## WebRTC

Only the **signalling** goes through Conduit: it is HTTP or WebSocket (see above). The **media** is UDP (ICE, DTLS, SRTP),
and Conduit has no UDP proxy, so media must reach the peers or the SFU/TURN server directly (open its UDP port range) or
through a TURN server. A TURN server that listens on TCP can be reached through a [TCP site](#what-a-tcp-site-does-and-does-not-do),
which Conduit relays like any TCP port (not tested).

## RTMP

RTMP (publish from OBS or ffmpeg, play from a media server) is a TCP stream on port 1935:

```yaml
sites:
  - port: 1935
    tcp: {targets: ["media.internal:1935"]}
```

The test completes the RTMP handshake through the site. A published stream can last for hours; there is no idle timeout in TCP
mode. RTMPS is RTMP inside TLS, which passes through untouched. HTTP-FLV and HLS output of the same media server are HTTP.

## MQTT

```yaml
sites:
  - port: 1883
    tcp: {targets: ["broker.internal:1883"]}
  - port: 8883                       # MQTT over TLS: passes through, the broker holds the certificate
    tcp: {targets: ["broker.internal:8883"]}
```

The test sends `CONNECT`, a QoS 1 `PUBLISH` and `PINGREQ` and checks the acknowledgements. MQTT over WebSocket is a WebSocket
route (`websocket: true`), not a TCP site. The broker sees every client as Conduit's address, so use unique client IDs, not
IP-based ACLs.

## PostgreSQL

```yaml
sites:
  - port: 5432
    tcp: {targets: ["pg.internal:5432"]}
```

The test opens a connection through the site with the `SSLRequest` first, as `psql` does; the answer comes from the server and
the startup, authentication and a query go through. TLS to PostgreSQL is end to end between client and server; Conduit
does not see it. Without a PROXY protocol the server sees Conduit's address in `pg_hba.conf` and `pg_stat_activity`.
Balancing is per connection; do not point it at a primary and a replica together.

## Redis

```yaml
sites:
  - port: 6379
    tcp: {targets: ["redis.internal:6379"]}
```

`PING`, `SET` and a 100 000-byte `GET` pass through in the test. Redis Cluster and Sentinel tell clients the addresses of the
nodes (`MOVED`, `ASK`), so those clients bypass a single proxy address: use Conduit in front of one standalone Redis (or
a managed endpoint), not in front of a cluster's nodes.

## SMTP

```yaml
sites:
  - port: 25
    tcp: {targets: ["mail.internal:25"]}
  - port: 587
    tcp: {targets: ["mail.internal:587"]}
```

`EHLO` and a full message transaction pass in the test (Python `smtplib`). `STARTTLS` and implicit TLS (465) are between
client and server and pass through; the `STARTTLS` exchange itself is not part of the test. The mail server sees Conduit's
address as the client, which matters for relay rules, rate limits, SPF checks and spam scoring on that server.

## FTP

FTP uses a **control** connection (port 21) and a separate **data** connection for every listing and transfer. In passive
mode the server tells the client where to connect for the data: in the reply to `PASV` (`227`) or `EPSV` (`229`). Conduit
does not rewrite that reply, so the server has to announce an address and port that reach **Conduit**, and Conduit needs a
TCP site for every passive port:

```yaml
sites:
  - port: 21
    tcp: {targets: ["ftp.internal:21"]}
  - port: 30000          # the server's passive range, one site per port
    tcp: {targets: ["ftp.internal:30000"]}
  - port: 30001
    tcp: {targets: ["ftp.internal:30001"]}
```

On the FTP server set the passive address to the Conduit host and a small passive range (for vsftpd: `pasv_address`,
`pasv_min_port`, `pasv_max_port`). The test does exactly that (server announces `app.test:2122`, Conduit has a site on
2122) and lists a directory with `ftplib`. **Active mode** (the server connects back to the client) does not work through
a proxy. SFTP is not FTP: see below.

## SFTP and SSH

SFTP, `scp`, `rsync -e ssh` and git over SSH are all SSH, a TCP stream:

```yaml
sites:
  - port: 22
    tcp: {targets: ["ssh.internal:22"]}
```

The test checks that the identification strings of client and server cross the proxy untouched. The client sees the upstream's
host key, so `known_hosts` entries made through Conduit carry the backend's key: keep one backend or give all backends the same host key.
If Conduit's own host needs SSH on port 22, put Conduit on another port (`ssh -p 2222`).

## What is not supported

These are gaps rather than choices; each is a candidate feature:

- UDP proxying (WebRTC media, QUIC, DNS, syslog).
- TLS termination for TCP sites, the PROXY protocol, an access filter (IP allow/deny), an idle timeout and a connection limit on TCP sites.
- Port ranges (FTP passive ports need one site each).
- Active health probes that speak h2c or TLS (a TLS upstream is probed by connecting only), or a TCP-level probe for TCP sites.
- A per-route custom CA for `upstreamTls` (only `verify: false` or the process-wide roots: the system store, or the file in `SSL_CERT_FILE`). The host needs a non-empty root store even with `verify: false`; see [Upstream TLS verification](configuration.md#upstream-tls-verification).
