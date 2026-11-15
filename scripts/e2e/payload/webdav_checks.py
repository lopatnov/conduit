"""Checks of the webdav.sh scenario: what a WebDAV client's requests look like when they reach the upstream.

usage: webdav_checks.py PROXIED_BASE STATIC_BASE   (for example http://app.test:8080 http://app.test:8081)

PROXIED_BASE has `/dav` (stripPrefix: the upstream is mounted at its root), `/keep` (the prefix stays) and `/rw` (a
rewrite rule). STATIC_BASE is a static site holding `hello.txt`.
"""

import hashlib
import http.client
import json
import sys
from urllib.parse import urlsplit


def fail(message):
    print("FAIL:", message)
    sys.exit(1)


def request(base, method, target, headers=None, body=None):
    parts = urlsplit(base)
    conn = http.client.HTTPConnection(parts.hostname, parts.port, timeout=10)
    conn.request(method, target, body=body, headers=headers or {})
    response = conn.getresponse()
    data = response.read()
    conn.close()
    return response.status, {k.lower(): v for k, v in response.getheaders()}, data


def echo(base, method, target, headers=None, body=None, expect=None):
    status, response_headers, data = request(base, method, target, headers, body)
    if expect is not None and status != expect:
        fail(f"{method} {target}: status {status}, expected {expect}")
    try:
        return status, response_headers, json.loads(data) if data else None
    except ValueError:
        fail(f"{method} {target}: the body is not the upstream's echo: {data[:200]!r}")


def same(what, got, want):
    if got != want:
        fail(f"{what}: got {got!r}, expected {want!r}")


def main():
    proxied, static = sys.argv[1], sys.argv[2]
    authority = urlsplit(proxied).netloc
    xml = '<?xml version="1.0"?><propfind xmlns="DAV:"><allprop/></propfind>'
    token = "<opaquelocktoken:6f1a2c34-0000-4000-8000-000000000001>"
    dav_headers = {
        "Depth": "1",
        "Overwrite": "F",
        "Timeout": "Second-3600",
        "Lock-Token": token,
        "Content-Type": 'application/xml; charset="utf-8"',
    }

    # 1. Every WebDAV method reaches the upstream with its status, body and headers intact.
    statuses = {"PROPFIND": 207, "PROPPATCH": 207, "MKCOL": 201, "COPY": 201, "MOVE": 201, "LOCK": 200, "UNLOCK": 204,
                "DELETE": 204, "PUT": 200, "GET": 200, "OPTIONS": 200}
    for method, expected in statuses.items():
        body = xml.encode() if method in ("PROPFIND", "PROPPATCH", "LOCK", "PUT") else None
        if expected == 204:
            status, response_headers, _ = request(proxied, method, "/dav/col/a.txt", dav_headers, body)
            same(f"{method} status", status, 204)
            # No body to echo: the upstream's receipt headers say what reached it.
            same(f"{method}: method seen by the upstream", response_headers.get("x-seen-method"), method)
            same(f"{method}: request target (prefix stripped)", response_headers.get("x-seen-target"), "/col/a.txt")
            same(f"{method}: Lock-Token", response_headers.get("x-seen-lock-token"), token)
            same(f"{method}: Depth", response_headers.get("x-seen-depth"), "1")
            same(f"{method}: content-type", response_headers.get("x-seen-content-type"), dav_headers["Content-Type"])
            same(f"{method}: Host is the client's", response_headers.get("x-seen-host"), authority)
            continue
        status, response_headers, seen = echo(proxied, method, "/dav/col/a.txt", dav_headers, body, expect=expected)
        same(f"{method}: method seen by the upstream", seen["method"], method)
        same(f"{method}: request target (prefix stripped)", seen["target"], "/col/a.txt")
        same(f"{method}: body", seen["body"], xml if body else "")
        for name in ("Depth", "Overwrite", "Timeout", "Lock-Token"):
            same(f"{method}: {name}", seen["headers"].get(name.lower()), dav_headers[name])
        same(f"{method}: content-type", seen["headers"].get("content-type"), dav_headers["Content-Type"])
        same(f"{method}: Host is the client's", seen["headers"]["host"], authority)
        same(f"{method}: Allow reaches the client", response_headers.get("allow"), "OPTIONS, GET, PUT, DELETE, PROPFIND, PROPPATCH, MKCOL, COPY, MOVE, LOCK, UNLOCK")
        if method == "OPTIONS":
            same("OPTIONS: DAV header reaches the client", response_headers.get("dav"), "1, 2")
        if method == "LOCK":
            same("LOCK: Lock-Token reaches the client", response_headers.get("lock-token"), token)
    print("ok: PROPFIND PROPPATCH MKCOL COPY MOVE LOCK UNLOCK DELETE PUT GET OPTIONS reach the upstream unchanged")

    # 2. Destination and the resource tags of If are the client's URLs, in the client's namespace: Conduit passes them on
    #    exactly as sent, whatever the route does to the request line. A server that knows its public prefix (Radicale with
    #    X-Script-Name, for one) needs them that way.
    sent_if = f"<http://{authority}/dav/col/a.txt> ({token} [\"etag\"]) </dav/col/b.txt> (Not <DAV:no-lock>)"
    for destination in (
        f"http://{authority}/dav/new%20dir/b.txt",
        "/dav/new%20dir/b.txt",
        f"https://elsewhere.example/other/b.txt",
    ):
        for method in ("COPY", "MOVE"):
            _, _, seen = echo(proxied, method, "/dav/a.txt", {"Destination": destination, "Overwrite": "T", "If": sent_if}, expect=201)
            same(f"{method} request target (prefix stripped)", seen["target"], "/a.txt")
            same(f"{method} Destination {destination}", seen["headers"]["destination"], destination)
            same(f"{method} If", seen["headers"]["if"], sent_if)
            same(f"{method} Overwrite", seen["headers"]["overwrite"], "T")
    print("ok: Destination and If reach the upstream exactly as the client sent them, with the request line stripped")

    # 3. A route that keeps its prefix, and one with a rewrite rule: the request line follows the route, the headers do not.
    _, _, seen = echo(proxied, "MOVE", "/keep/a.txt", {"Destination": f"http://{authority}/keep/b.txt"}, expect=201)
    same("/keep target", seen["target"], "/keep/a.txt")
    same("/keep Destination", seen["headers"]["destination"], f"http://{authority}/keep/b.txt")
    _, _, seen = echo(proxied, "COPY", "/rw/a.txt", {"Destination": f"http://{authority}/rw/b.txt"}, expect=201)
    same("/rw target", seen["target"], "/store/a.txt")
    same("/rw Destination", seen["headers"]["destination"], f"http://{authority}/rw/b.txt")
    print("ok: /keep keeps its prefix; /rw rewrites the request line only")

    # 3a. The opt-in `rewriteDestination`: the same strip and rewrite rules are applied to the paths in Destination and in the
    #     resource tags of If; the authority, the query, the state tokens and entity tags are not touched, a URL of another
    #     authority is left alone, and the default routes above are unchanged.
    sent_if = f"<http://{authority}/mv/col/a.txt> ({token} [\"/mv/etag\"]) </mv/col/b.txt> (Not <DAV:no-lock>)"
    want_if = f"<http://{authority}/col/a.txt> ({token} [\"/mv/etag\"]) </col/b.txt> (Not <DAV:no-lock>)"
    for destination, want in (
        (f"http://{authority}/mv/new%20dir/b.txt?x=1", f"http://{authority}/new%20dir/b.txt?x=1"),
        ("/mv/new%20dir/b.txt", "/new%20dir/b.txt"),
        ("https://elsewhere.example/mv/b.txt", "https://elsewhere.example/mv/b.txt"),
        (f"http://{authority}/outside/b.txt", f"http://{authority}/outside/b.txt"),
    ):
        for method in ("COPY", "MOVE"):
            _, _, seen = echo(proxied, method, "/mv/a.txt", {"Destination": destination, "If": sent_if}, expect=201)
            same(f"{method} /mv request target", seen["target"], "/a.txt")
            same(f"{method} /mv Destination {destination}", seen["headers"]["destination"], want)
            same(f"{method} /mv If", seen["headers"]["if"], want_if)
    _, _, seen = echo(proxied, "COPY", "/rwd/a.txt", {"Destination": f"http://{authority}/rwd/b.txt"}, expect=201)
    same("/rwd target", seen["target"], "/store/a.txt")
    same("/rwd Destination", seen["headers"]["destination"], f"http://{authority}/store/b.txt")
    _, _, seen = echo(proxied, "GET", "/mv/a.txt", expect=200)
    if "destination" in seen["headers"] or "if" in seen["headers"]:
        fail("rewriteDestination invented a Destination or If header")
    print("ok: rewriteDestination maps Destination and the resource tags of If like the request path, and nothing else")

    # 3b. The response side is passed on as the upstream wrote it: the hrefs of a multistatus body and the Location of a 201.
    status, response_headers, data = request(proxied, "PROPFIND", "/dav/multistatus", {"Depth": "1"}, xml.encode())
    same("multistatus status", status, 207)
    same("multistatus content-type", response_headers.get("content-type"), 'application/xml; charset="utf-8"')
    if b"<D:href>/dav/col/</D:href>" not in data or b"<D:href>/dav/col/a%20b.txt</D:href>" not in data:
        fail(f"the multistatus hrefs were changed: {data!r}")
    status, response_headers, _ = request(proxied, "MKCOL", "/dav/located")
    same("MKCOL status", status, 201)
    same("MKCOL Location", response_headers.get("location"), "http://app.test:8080/dav/created%20dir/")
    print("ok: multistatus hrefs and the Location of a 201 come back exactly as the upstream wrote them")

    # 4. A large PUT body is relayed whole.
    payload = bytes(range(256)) * 4096
    status, _, data = request(proxied, "PUT", "/dav/big.bin", {"Content-Type": "application/octet-stream"}, payload)
    same("1 MiB PUT status", status, 200)
    seen = json.loads(data)
    same("1 MiB PUT body length", seen["body_length"], len(payload))
    same("1 MiB PUT body hash", seen["body_sha256"], hashlib.sha256(payload).hexdigest())
    print("ok: a 1 MiB PUT reaches the upstream whole (length and SHA-256 match)")

    # 5. A static site is read-only: the file is served for GET and HEAD, every other method is refused with 405 and Allow.
    status, _, data = request(static, "GET", "/hello.txt")
    same("static GET", (status, data), (200, b"hello\n"))
    for method in ("PROPFIND", "PROPPATCH", "MKCOL", "COPY", "MOVE", "LOCK", "UNLOCK", "PUT", "DELETE", "POST"):
        status, headers, _ = request(static, method, "/hello.txt", {"Depth": "0"})
        same(f"static {method} status", status, 405)
        same(f"static {method} Allow", headers.get("allow"), "GET, HEAD, OPTIONS")
    print("ok: a static site refuses the methods that write or browse (405 with Allow) and still serves GET")

    print("webdav checks passed")


main()
