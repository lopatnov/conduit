"""A WebDAV-shaped upstream for the webdav.sh scenario (standard library only).

It answers every request, whatever the method, with the status a WebDAV server gives (207 for PROPFIND and PROPPATCH,
201 for MKCOL, COPY and MOVE, 200 for LOCK, 204 for UNLOCK and DELETE), the `DAV`, `Lock-Token` and `Allow` headers a
client reads, `X-Seen-*` headers naming the method, target, `Lock-Token`, `Depth`, `Content-Type` and `Host` it received (for the
answers without a body), and a JSON body that echoes what it received: method, request target, headers, body, and the body's
length and SHA-256. The checks then
compare that echo with what the client sent, so a header that was cut, renamed or rewritten shows up.
"""

import hashlib
import json
import os
import socket
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Two fixed answers for the checks that the response side is passed on as the upstream wrote it: a multistatus body whose
# hrefs name the client's namespace (the upstream was told its public prefix), and a Location in that namespace.
MULTISTATUS = (
    b'<?xml version="1.0" encoding="utf-8"?>'
    b'<D:multistatus xmlns:D="DAV:"><D:response><D:href>/dav/col/</D:href></D:response>'
    b'<D:response><D:href>/dav/col/a%20b.txt</D:href></D:response></D:multistatus>'
)
LOCATION = "http://app.test:8080/dav/created%20dir/"

STATUS = {"PROPFIND": 207, "PROPPATCH": 207, "MKCOL": 201, "COPY": 201, "MOVE": 201, "LOCK": 200, "UNLOCK": 204, "DELETE": 204}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def handle_any(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        if self.command == "PROPFIND" and self.path == "/multistatus":
            self.send_response(207)
            self.send_header("Content-Type", 'application/xml; charset="utf-8"')
            self.send_header("Content-Length", str(len(MULTISTATUS)))
            self.end_headers()
            self.wfile.write(MULTISTATUS)
            return
        echo = json.dumps(
            {
                "method": self.command,
                "target": self.path,
                "headers": {name.lower(): value for name, value in self.headers.items()},
                "body": body.decode("utf-8", "replace"),
                "body_length": len(body),
                "body_sha256": hashlib.sha256(body).hexdigest(),
            }
        ).encode()
        status = STATUS.get(self.command, 200)
        self.send_response(status)
        self.send_header("DAV", "1, 2")
        self.send_header("Allow", "OPTIONS, GET, PUT, DELETE, PROPFIND, PROPPATCH, MKCOL, COPY, MOVE, LOCK, UNLOCK")
        # A receipt of what arrived, in headers, for the answers that have no body (204).
        self.send_header("X-Seen-Method", self.command)
        self.send_header("X-Seen-Target", self.path)
        for name in ("Lock-Token", "Depth", "Content-Type", "Host"):
            self.send_header(f"X-Seen-{name}", self.headers.get(name, ""))
        if self.command == "MKCOL" and self.path == "/located":
            self.send_header("Location", LOCATION)
        if self.command == "LOCK":
            self.send_header("Lock-Token", "<opaquelocktoken:6f1a2c34-0000-4000-8000-000000000001>")
        if status != 204:
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(echo)))
        else:
            self.send_header("Content-Length", "0")
        self.end_headers()
        if status != 204 and self.command != "HEAD":
            self.wfile.write(echo)

    do_GET = do_HEAD = do_PUT = do_POST = do_DELETE = do_OPTIONS = handle_any
    do_PROPFIND = do_PROPPATCH = do_MKCOL = do_COPY = do_MOVE = do_LOCK = do_UNLOCK = handle_any

    def log_message(self, *args):
        pass


def bind_address():
    """The address of this container: the server listens there and not on every interface (BIND_ADDRESS overrides)."""
    return os.environ.get("BIND_ADDRESS") or socket.gethostbyname(socket.gethostname())


if __name__ == "__main__":
    ThreadingHTTPServer((bind_address(), int(sys.argv[1])), Handler).serve_forever()
