# External processing (`ext_proc`)

An external processor is a separate gRPC service, written in any language, that Conduit asks about a request (and,
optionally, its body and the response). It can read the request, set or remove headers, rewrite the request body,
change or replace the response, or answer the request itself. It speaks the
[`envoy.service.ext_proc.v3.ExternalProcessor`](https://www.envoyproxy.io/docs/envoy/latest/api-v3/service/ext_proc/v3/external_processor.proto)
protocol of Envoy, so a processor written for Envoy (an OPA or WAF filter, a DLP service) can serve Conduit largely unchanged (what is not
implemented is listed under [What is not there](#what-is-not-there)).

New to middleware? [Middleware overview](middleware.md) says how the `middleware` list behaves and when to choose a
built-in check, a Rhai script, a plugin or a processor.

**Which installs have it.** `ext_proc` is the `ext-proc` Cargo feature, part of the `full` bundle: the plain-named
release downloads, the `:latest` image and the npm package contain it; the `-standard` binary and the default build do
not (see [Optional build features](configuration.md#optional-build-features)). Unlike a script or a plugin, **a build
without the feature refuses to start with a fail-closed entry** (the default), because silently dropping a check that
was meant to be a gate is the worse failure; a `failOpen: true` entry is ignored with a warning.

> **A processor that fails refuses the request (fail-closed).** A processor that cannot be reached, is later than
> `timeoutMs`, closes its stream, answers with an error or answers with something Conduit does not accept makes Conduit
> answer `503` (`statusOnError`) instead of forwarding the request. This is the opposite of Rhai and WASM, which let the
> request through. Set `failOpen: true` only for a processor that is allowed to be skipped (tagging, logging), never
> for one that guards something.

---

## Contents

- [Limits and guarantees](#limits-and-guarantees)
- [Your first processor](#your-first-processor)
- [Configuration](#configuration)
- [The protocol](#the-protocol)
- [Recipes](#recipes)
- [Testing and debugging](#testing-and-debugging)
- [Stability](#stability)
- [Comparison with Rhai and WASM](#comparison-with-rhai-and-wasm)

---

## Limits and guarantees

| | |
| --- | --- |
| **Fail-closed** | Unreachable, late (`timeoutMs`, default 200 ms), a closed stream, a gRPC error or an answer that is not accepted: the request is refused with `statusOnError` (default `503`). With `failOpen: true` it continues unchanged, and the entry then does nothing for the rest of that request. A refusal stays a refusal: if a route's `retry` produces another response for the same request, it is refused again. |
| **What may be changed** | Request and response headers, the request body, the response (whole). Never: pseudo-headers, `host`, `content-length`, `transfer-encoding`, `connection`, `keep-alive`, `proxy-connection`, `te`, `trailer`, `upgrade` (they frame or route the message; changing them is the ingredient of request smuggling). |
| **Bad answers** | A header name that is not valid, a value with a control character, an unknown action, more than `maxHeaderMutations` changes, a value over `maxHeaderValueBytes`, a status outside 200 to 599: the whole answer is refused (a failure), half of a mutation is never applied. |
| **Request body** | `buffered` holds the body up to `maxBodyBytes` (default 1 MiB, at most 16 MiB; 413 above) and sends it as one message; `streamed` sends every chunk as it arrives. A rewritten body is forwarded with chunked framing. |
| **Response body** | Observe-only: a copy of every chunk is sent to the processor and never waited for. It cannot be rewritten (use a [WASM](wasm.md) `on_response` plugin for that). |
| **Trailers** | Not sent; an answer that carries one is refused. |
| **One stream per request** | Messages go in order and each is answered before the next is sent (except the observed response body). The stream lives as long as the request. |
| **Trust** | The processor is trusted: it can set any header that is not in the list above, including headers the upstream treats as identity, and it receives every request header, `Authorization` and cookies included. Run it on loopback or a private network, or use `grpcs://`. |

---

## Your first processor

A processor in Python that refuses `/admin` with `403`, tags every other request, and removes the `X-Internal` header the
client may have sent. The full file is [`examples/ext-proc/processor.py`](../examples/ext-proc/processor.py); it also
refuses a request body that contains `DROP TABLE` and removes the `Server` header from responses, to show the other phases.
You need Python 3 and the messages Envoy generates from its own protocol files (the `xds-protos` package):

```bash
pip install grpcio==1.85.1 xds-protos==1.85.1 protobuf==7.36.2 typing_extensions==4.16.0
python3 examples/ext-proc/processor.py 50051
```

The heart of it, for the request headers phase:

```python
class Processor(services.ExternalProcessorServicer):
    def Process(self, request_iterator, context):
        for request in request_iterator:                       # one message per phase, in order
            if request.WhichOneof("request") == "request_headers":
                # a demonstration: real path rules must normalise the path first (//admin, %2e, dot segments)
                if header_value(request.request_headers, ":path").startswith("/admin"):
                    yield refuse(403, "admin is not for you\n")  # ends the request with this answer
                    return
                yield pb.ProcessingResponse(
                    request_headers=pb.HeadersResponse(
                        response=pb.CommonResponse(
                            header_mutation=pb.HeaderMutation(
                                set_headers=[set_header("x-checked-by", "processor.py")],
                                remove_headers=["x-internal"],
                            )
                        )
                    )
                )
```

Point Conduit at it:

```yaml
port: 8080
proxy: "http://127.0.0.1:4000"
middleware:
  - type: ext_proc
    config:
      grpc: "grpc://127.0.0.1:50051"
      requestBody: buffered      # the body phase of the sample
      responsePhase: true        # the response phase of the sample
```

Start your upstream on port 4000 and try it:

```
$ curl -i http://127.0.0.1:8080/admin/users
HTTP/1.1 403 Forbidden
...
admin is not for you

$ curl -s -H 'X-Internal: 1' http://127.0.0.1:8080/anything     # the upstream sees x-checked-by, and no x-internal
$ curl -s -X POST --data 'DROP TABLE users' http://127.0.0.1:8080/anything
that body is refused
```

Stop the processor and the same requests answer `503`: that is fail-closed at work.

---

## Configuration

```yaml
middleware:
  - type: ext_proc
    config:
      grpc: "grpc://127.0.0.1:50051"   # or grpcs://host:port (TLS)
      timeoutMs: 200
      failOpen: false
```

Every key, with its default, is in the table of [External processing in the configuration
reference](configuration.md#external-processing-ext_proc). `conduit validate` rejects an unknown key, a port that is
missing from `grpc`, a `timeoutMs` outside 1 to 60000 and the other values that cannot work, naming the field.

Entries run **in the order written**, mixed with `script` and `wasm` entries, after the built-in guards (IP filter, rate
limit, authentication, `forwardAuth`, redirects, fault injection), so the processor only sees requests that already passed
them. The first entry that ends the request ends the chain. A `phase` on the entry has no effect (validation warns):
the phases an entry takes part in are chosen by its own keys (`requestPhase`, `requestBody`, `responsePhase`,
`responseBody`). There is no per-route override, because Conduit has no per-route `middleware`: an entry belongs to a
site, so put the routes that need a different processor on their own site.

---

## The protocol

### What is sent
 One stream per request, with the messages in step (a message is sent and its answer awaited, within
`timeoutMs`), as in Envoy's protocol:

1. `request_headers` (unless `requestPhase: false`): the pseudo-headers `:method`, `:scheme`, `:authority` (the `Host`)
   and `:path` (path and query) followed by every request header, values as `raw_value`, and `end_of_stream` set when
   the request has no body. `:scheme` is `https` when the client connected over TLS to Conduit.
2. `request_body` (with `requestBody`): `buffered` is one message with the whole body and `end_of_stream: true` (an
   empty body is sent too, as an empty one); `streamed` is one message per chunk, the last with `end_of_stream: true`.
3. `response_headers` (with `responsePhase`): `:status` and the upstream's response headers, before the site's own
   headers (CORS, security, `responseTransform`) are added, so a processor can neither see nor strip those.
4. `response_body` (with `responseBody: observe`): a copy of each chunk, `end_of_stream` on the last.

An entry that sends only the request headers (the default) uses one short stream per request; an entry with any later
phase keeps its stream open until the request ends.

### What is applied


- *A header mutation* (`request_headers` answer): each `remove_headers` name first, then each `set_headers` entry (so a
  header that is removed and set again stays set, as in Envoy). A set replaces an existing value unless the entry says
  `append: true`, which adds a value (Envoy's `append` defaults to false in this message); an entry that names an
  `append_action` other than the default (`ADD_IF_ABSENT`, `OVERWRITE_IF_EXISTS_OR_ADD`, `OVERWRITE_IF_EXISTS`) gets
  exactly that. A set with an empty value is skipped
  unless `keep_empty_value` is set; for an overwrite that means the header is **removed** (Envoy's rule). Later middleware
  entries see the changed headers.
- *A body mutation* (`request_body` answer): the `body` it carries replaces the body (or the chunk, when streamed);
  `clear_body: true` empties it; an answer with no mutation keeps it. The request is then forwarded with **chunked
  framing** (`Transfer-Encoding: chunked` to an HTTP/1.1 upstream, no `Content-Length` to an HTTP/2 one), because the
  length of what is forwarded is no longer the length of what the client sent. A body mutation in a *headers* answer, a
  header mutation in a *body* answer, and any answer to `response_body` are ignored and logged. A request with no body and a
  method that carries none (`GET`, `HEAD`, `DELETE`) is not shown to the body phases at all (the headers phase already
  said `end_of_stream`); an empty `POST`, `PUT` or `PATCH` is, and a processor may supply its body.
- *A response-headers answer* (`responsePhase`): the same header rules as for the request, applied to the upstream's
  response. Entries run in order, each shown the headers the one before left. The site's own response headers are added
  afterwards.
- *An immediate response*: the request ends there with the processor's status (200 to 599), headers and body; the
  site's CORS and security headers are added. One value per header name (a second `Set-Cookie`, even with an append
  action, replaces the first). 204 and 304 are sent without a body and without `Content-Length`; a 205 has `Content-Length: 0`. In the
  **response** phase an immediate response replaces the upstream's status, headers and body; when the upstream response
  has no body to replace (a `HEAD` request, 204, 304, `Content-Length: 0`) the answer's status and headers are sent and its
  body is not (logged once).
- *Headers the processor may not touch*: pseudo-headers, `host`, `content-length`, `transfer-encoding`, `connection`,
  `keep-alive`, `proxy-connection`, `te`, `trailer` and `upgrade`. They frame or route the request, and changing them is
  the ingredient of request smuggling. A mutation of one is ignored and logged, as Envoy does by default.
- *An answer that is not well formed* (a bad header name, a value with a control character, an unknown action, more
  mutations than `maxHeaderMutations`, a value over `maxHeaderValueBytes`, an immediate response status outside 200 to
  599, an answer for a phase that was not asked about) is refused **as a whole**: half of a mutation is not applied. It
  counts as a failure of the processor.

### When the processor fails

**When the processor fails** (it cannot be reached, closes the stream without answering, answers with a gRPC error, is
later than `timeoutMs`, or sends an answer that is not accepted) the entry's policy decides: refused with
`statusOnError`, or, with `failOpen: true`, the request continues unchanged. A failure ends the stream: with `failOpen`
the entry then does nothing for the rest of that request (a request body that was being held is forwarded as it was
received); without it the request is refused, or, if the response is already being sent, the response is replaced by
the refusal. Each failure is logged at `warn` level on the `conduit::ext_proc` target with the reason. `observe` never
fails a request: a processor that cannot keep up (the 256-chunk queue is full, or a send takes longer than `timeoutMs`) or is
unreachable simply stops being fed. A response that a fail-closed entry refused stays refused: if the route's `retry` (or
anything else) produces another response for the same request, the refusal is repeated for it, and a response replaced by
a processor's answer is never retried upstream.

### Where it runs

**Where it runs.** `ext_proc` entries run in `middleware` order, after the built-in guards (IP filter, rate limit,
authentication, `forwardAuth`, redirects, fault injection), so the processor only sees requests that already passed
them. The processor is trusted: it can set any header that is not in the list above, including headers that the upstream
treats as identity, and a token it sets in `Authorization` is read, unverified, by features that run after the guards
(the `{{ jwt.<claim> }}` header templates). It receives every request header, `Authorization` and cookies included, so
`grpc://` (plain text) belongs on loopback or a private network only. Do not point `grpc` at a service you do not control.

### Where the response phase does not run

**Where the response phase does not run.** The response phase and `responseBody` run for responses that come from an
upstream. They do not run for answers Conduit gives itself (cache hits, static files, redirects, health and other local
handlers), so a fail-closed `responsePhase` entry does not apply to those. A response stored in the cache is stored as the
processor changed it, and the cache key does not include headers a processor set on the request. The processor is shown the
upstream's response headers, `Set-Cookie` included. With `requestBody` and `responseBody: observe` together, a response
that starts before the client has finished its upload is not observed (the stream is still in use for the upload).
`buffered` holds the body, not the request headers: the upstream has already received them when a processor refuses the
body, and sees the body end early. Once a connection has been upgraded (the upstream answered `101`, WebSocket) the bytes of the
tunnel are not sent to the body phases: they are not a request body. A request that merely carries an `Upgrade`
header is an ordinary request until then, and its body is processed.

### What is not there

**What is not there.** Trailers (`request_trailers`/`response_trailers`) are not sent; a processor that answers with one
is refused (a failure). `response_body` is observe-only: the response body cannot be rewritten through `ext_proc` (the
body hooks of the proxy are synchronous; use a WASM `on_response` plugin to replace a response body). There is no
per-route override because Conduit has no per-route `middleware`: an entry belongs to a site (put the routes that need
it on their own site). A `phase` on the entry has no effect (validation warns): the phases are the entry's own keys.
`requestBody` and a retry: a request that is retried before its body was read is simply asked again; Conduit does not
replay a processed body.

### Connections

**Connections.** One HTTP/2 channel per processor address is shared by all requests; it connects on the first request and
reconnects on its own. A configuration reload drops the channels, so a moved processor or an edited `caFile` takes effect
without a restart.

---

## Recipes

**Deny by path.** Answer from the request headers phase with an immediate response: `refuse(403, "...")` above. The
request never reaches the upstream, and the site's CORS and security headers are added to the answer. The sample compares the
raw `:path`, which is a demonstration, not access control: `//admin`, percent-encoding and dot segments reach the same
resource on many upstreams. A processor that decides by path must normalise it first, and anything that guards something
belongs in the built-in checks (`consumers`, `jwtAuth`, `forwardAuth`, `ipFilter`) as well.

**Scan or redact a body.** Set `requestBody: buffered` and look at the one `request_body` message (the whole body, up to
`maxBodyBytes`). Answer with an empty `BodyResponse` to keep it, with a `body_mutation` to replace it
(`CommonResponse(body_mutation=BodyMutation(body=redacted))`), or with an immediate response to refuse it. A body over
`maxBodyBytes` is refused with `413` before the processor sees it. `requestBody: streamed` does the same per chunk, for
bodies too big to hold; the last chunk has `end_of_stream` set.

**Add the caller's identity.** Read `authorization` or a cookie from the request headers, look the caller up in your own
system and answer with `set_headers=[set_header("x-user", user)]`. Remove any `x-user` the client sent in the same
answer (`remove_headers=["x-user"]`) so that only your value reaches the upstream.

**Mask an upstream error.** Set `responsePhase: true`. The processor is sent `:status` and the upstream's headers; answer
a `500` with an immediate response of your own (`refuse(503, "try again later")`) and the client never sees the
upstream's answer. The site's own CORS and security headers are added afterwards, and a route's `retry` does not retry a
response that the processor replaced.

**Count what leaves.** Set `responseBody: observe`: every chunk of the response body is copied to the processor, which
can count bytes or scan for secrets and raise an alarm. Nothing it answers is acted on, and a processor that cannot keep up
simply stops being fed; the client's response is never delayed.

---

## Testing and debugging

**Before it runs: `conduit validate`.** It reports a config that cannot work, naming the field:

```
$ conduit validate -c conduit.yaml
error at sites[0].middleware[0].config.grpc: grpc "127.0.0.1:50051" must start with grpc:// (plain text) or grpcs:// (TLS)
```

In a build without the feature a fail-closed entry is an error and a `failOpen` entry a warning (see above). What
`validate` cannot tell is whether the processor is reachable or decides correctly; Conduit does not connect at start-up.

**Seeing what happens.** Every failure is logged at `warn` level on the `conduit::ext_proc` target, with the processor and
the reason, for example:

```
WARN conduit::ext_proc: external processing failed; the request is refused processor=grpc://127.0.0.1:50051 reason=the processor did not answer within 200 ms status=503
WARN conduit::ext_proc: external processing failed; the request continues unchanged (failOpen) processor=... reason=the processor closed the stream without answering
```

Raise `timeoutMs` while you debug a processor under a breakpoint. A processor that answers with something Conduit does not
accept (a bad header name or value, too many changes) is a failure with that reason in the log; a mutation of a header in
the forbidden list is ignored with a log line, not refused.

**Testing a processor.** Your processor is a plain gRPC server: call it with any gRPC client and the generated Envoy messages
before you put Conduit in front of it. For the whole chain, `scripts/e2e/ext-proc.sh BINARY` in this repository runs Conduit,
an upstream and a processor built from Envoy's own messages as separate processes and checks every phase; it is the
reference for what a processor can rely on.

---

## Stability

The protocol is Envoy's. What Conduit does not implement ([What is not there](#what-is-not-there)) is refused when a
processor asks for it (trailers), is not offered (per-route overrides), or, for the response body, is **ignored and logged**:
`responseBody: observe` never fails a request and never changes the body, so an answer that tries to redact it has no
effect (rewrite a response body with a WASM `on_response` plugin). The messages are hand-written to Envoy's field numbers and pinned by tests against Envoy's own generated messages.
The configuration keys of this guide are what this release ships; like the rest of the 2.x configuration they change only
with a note in [CHANGELOG.md](../CHANGELOG.md).

---

## Comparison with Rhai and WASM

| | Rhai | WASM | ext_proc |
| --- | --- | --- | --- |
| Compile-time feature | `rhai` | `wasm` | `ext-proc` |
| Where the logic runs | in Conduit | in Conduit, sandboxed | in your own service (any language, any host) |
| Change request headers | no | yes | yes |
| Request body | no | no | yes: buffered or streamed, rewrite or refuse |
| Response | headers (phase `response`) | headers, status, body | headers, status, whole response; body observed only |
| State across requests | none | none | whatever your service keeps |
| Cost per request | in-process | in-process | a gRPC round trip (bounded by `timeoutMs`) |
| On failure | fail-open | fail-open | **fail-closed** (`failOpen` opt-in) |
| Development | no build step | a build step | a service to deploy and keep running |

Use a **built-in check** for IP, credentials, JWT, rate limits and redirects. Use **Rhai** for a small rule on headers or
the path, **WASM** to run compiled logic inside Conduit, and **ext_proc** when the logic already lives in a service (an
existing Envoy processor, a policy engine, a DLP scanner), needs state or a database, or must see the request body, and you
accept a network hop per request. See the [Rhai guide](rhai.md) and the [WASM guide](wasm.md).
