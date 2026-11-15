#!/usr/bin/env python3
"""Servers and clients for the streaming e2e scenario (Server-Sent Events and WebSocket), standard library only.

    streaming.py sse-server PORT            events, a stream that goes quiet, the request header it saw
    streaming.py sse-client URL [--max-seconds N] [--header 'Name: value']
    streaming.py ws-server PORT             echo; `close:CODE`, `sleep:SECONDS`, `ping` commands (see handle_text)
    streaming.py ws-client URL SCRIPT       SCRIPT is a JSON list of steps (see ws_client); WS_HEADERS adds request headers

The clients print one JSON object on stdout, so they can be tried by hand; `streaming_checks.py` imports them.
Nothing here depends on the proxy under test: the same servers and clients run against the upstream directly.
"""
import base64
import hashlib
import http.client
import json
import os
import socket
import socketserver
import ssl
import struct
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


# ── Server-Sent Events ────────────────────────────────────────────────────────────────────────────────────────────
class SseHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "sse"
    sys_version = ""

    def log_message(self, *args):
        pass

    def chunk(self, text):
        data = text.encode()
        self.wfile.write(b"%x\r\n" % len(data) + data + b"\r\n")
        self.wfile.flush()

    def do_GET(self):
        url = urllib.parse.urlparse(self.path)
        q = urllib.parse.parse_qs(url.query)
        gap = float(q.get("gap", ["0.3"])[0])
        count = int(q.get("n", ["5"])[0])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        if url.path == "/echo-header":
            # what the upstream saw of the request, as one event: Last-Event-ID is how a client resumes a stream
            seen = {"last-event-id": self.headers.get("Last-Event-ID"), "accept": self.headers.get("Accept")}
            self.chunk("data: " + json.dumps(seen) + "\n\n")
        elif url.path == "/quiet":
            # one event, a silence of `gap` seconds, one more event: what an idle-timeout does to a stream
            self.chunk("data: first\n\n")
            time.sleep(gap)
            self.chunk("data: second\n\n")
        else:
            for i in range(count):
                self.chunk(f"id: {i}\nevent: tick\ndata: tick {i}\n\n")
                time.sleep(gap)
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()
        self.close_connection = True


def sse_client(url, max_seconds, headers=()):
    parts = urllib.parse.urlparse(url)
    cls = http.client.HTTPSConnection if parts.scheme == "https" else http.client.HTTPConnection
    kwargs = {"context": ssl.create_default_context(cafile=os.environ.get("CA_FILE"))} if parts.scheme == "https" else {}
    conn = cls(parts.hostname, parts.port, timeout=max_seconds, **kwargs)
    start = time.monotonic()
    out = {"events": [], "error": None}
    try:
        conn.request("GET", parts.path + ("?" + parts.query if parts.query else ""),
                     headers={"Accept": "text/event-stream", **dict(h.split(": ", 1) for h in headers)})
        resp = conn.getresponse()
        out["status"] = resp.status
        out["content_type"] = resp.getheader("Content-Type")
        out["content_encoding"] = resp.getheader("Content-Encoding")
        data = []
        while True:
            line = resp.readline()
            if not line:
                break
            line = line.decode().rstrip("\r\n")
            if line == "":
                if data:
                    out["events"].append({"data": "\n".join(data), "at": round(time.monotonic() - start, 2)})
                data = []
            elif line.startswith("data:"):
                data.append(line[5:].lstrip(" "))
        out["ended"] = "stream closed"
    except (socket.timeout, TimeoutError):
        out["ended"] = "client timeout"
    except (http.client.HTTPException, OSError) as e:
        out["ended"] = "error"
        out["error"] = f"{type(e).__name__}: {e}"
    out["seconds"] = round(time.monotonic() - start, 2)
    return out


# ── WebSocket (RFC 6455) ──────────────────────────────────────────────────────────────────────────────────────────
def read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        part = sock.recv(n - len(buf))
        if not part:
            raise ConnectionError("closed")
        buf += part
    return buf


def read_frame(sock):
    """One frame: (fin, opcode, payload). Client frames are masked, server frames are not."""
    b1, b2 = read_exact(sock, 2)
    fin, opcode = bool(b1 & 0x80), b1 & 0x0F
    masked, length = bool(b2 & 0x80), b2 & 0x7F
    if length == 126:
        (length,) = struct.unpack(">H", read_exact(sock, 2))
    elif length == 127:
        (length,) = struct.unpack(">Q", read_exact(sock, 8))
    mask = read_exact(sock, 4) if masked else None
    payload = read_exact(sock, length)
    if mask:
        payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return fin, opcode, payload


def write_frame(sock, opcode, payload=b"", mask=False, fin=True):
    head = bytes([(0x80 if fin else 0) | opcode])
    n = len(payload)
    flag = 0x80 if mask else 0
    if n < 126:
        head += bytes([flag | n])
    elif n < 65536:
        head += bytes([flag | 126]) + struct.pack(">H", n)
    else:
        head += bytes([flag | 127]) + struct.pack(">Q", n)
    if mask:
        key = os.urandom(4)
        payload = bytes(b ^ key[i % 4] for i, b in enumerate(payload))
        head += key
    sock.sendall(head + payload)


def read_message(sock, mask_replies):
    """A whole message (fragments joined); control frames in between are answered or returned as they come.

    Returns (opcode, payload) of a data message, or (8, payload) for close, or (10, payload) for a pong.
    """
    message, first = b"", None
    while True:
        fin, opcode, payload = read_frame(sock)
        if opcode == 9:  # ping: answer with the same payload
            write_frame(sock, 10, payload, mask=mask_replies)
            continue
        if opcode in (8, 10):
            return opcode, payload
        if first is None:
            first = opcode
        message += payload
        if fin:
            return first, message


class WsHandler(socketserver.BaseRequestHandler):
    def handle(self):
        sock = self.request
        head = b""
        while b"\r\n\r\n" not in head:
            part = sock.recv(4096)
            if not part:
                return
            head += part
        lines = head.decode("latin-1").split("\r\n")
        headers = {k.lower(): v for k, v in (line.split(": ", 1) for line in lines[1:] if ": " in line)}
        if headers.get("upgrade", "").lower() != "websocket":
            sock.sendall(b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        accept = base64.b64encode(hashlib.sha1((headers["sec-websocket-key"] + GUID).encode()).digest()).decode()
        # the Origin and Host the upstream saw are returned in the first message, so a scenario can check them
        seen = {"host": headers.get("host"), "origin": headers.get("origin"), "path": lines[0].split(" ")[1],
                "x-forwarded-for": headers.get("x-forwarded-for")}
        sock.sendall(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                      f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode())
        self.seen = "seen:" + json.dumps(seen)
        try:
            while True:
                opcode, payload = read_message(sock, mask_replies=False)
                if opcode == 8:
                    write_frame(sock, 8, payload[:2])
                    return
                if opcode == 1 and self.handle_text(sock, payload.decode()):
                    continue
                write_frame(sock, opcode, payload)
        except ConnectionError:
            return

    def handle_text(self, sock, text):
        """Commands: `seen?` answers with the Host, Origin, path and X-Forwarded-For of the upgrade request; `close:CODE`
        ends the connection from this side; `sleep:S` waits S seconds, then answers."""
        if text == "seen?":
            write_frame(sock, 1, self.seen.encode())
            return True
        if text.startswith("close:"):
            write_frame(sock, 8, struct.pack(">H", int(text[6:])) + b"bye")
            return True
        if text.startswith("sleep:"):
            time.sleep(float(text[6:]))
            write_frame(sock, 1, b"slept")
            return True
        return False


class ThreadingTCP(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    daemon_threads = True


def ws_client(url, steps, extra_headers=""):
    """Run the steps and return what came back. Steps (dicts):

    {"send": "text"} {"binary_mb": 1} {"ping": "payload"} {"fragmented": "text"} {"close": 1000} {"read": true}
    {"sleep": 2} {"recv_timeout": 5}
    Replies are collected in order; `binary_mb` reports a SHA-256 of what was sent and of what came back.
    """
    parts = urllib.parse.urlparse(url)
    port = parts.port or (443 if parts.scheme == "wss" else 80)
    raw = socket.create_connection((parts.hostname, port), timeout=10)
    sock = raw
    if parts.scheme == "wss":
        sock = ssl.create_default_context(cafile=os.environ.get("CA_FILE")).wrap_socket(raw, server_hostname=parts.hostname)
    key = base64.b64encode(os.urandom(16)).decode()
    path = parts.path or "/"
    extra = extra_headers
    sock.sendall((f"GET {path} HTTP/1.1\r\nHost: {parts.hostname}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                  f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n{extra}\r\n").encode())
    head = b""
    while b"\r\n\r\n" not in head:
        part = sock.recv(4096)
        if not part:
            break
        head += part
    header, _, rest = head.partition(b"\r\n\r\n")
    status = int(header.split(b" ")[1]) if header else 0
    out = {"status": status, "replies": []}
    if status != 101:
        out["body"] = rest.decode("latin-1")[:200]
        return out
    accept = base64.b64encode(hashlib.sha1((key + GUID).encode()).digest()).decode()
    out["accept_ok"] = f"Sec-WebSocket-Accept: {accept}".lower() in header.decode("latin-1").lower()
    assert not rest, "the server spoke before the handshake ended"
    start = time.monotonic()
    try:
        for step in steps:
            if "send" in step:
                write_frame(sock, 1, step["send"].encode(), mask=True)
            elif "binary_mb" in step:
                blob = os.urandom(step["binary_mb"] * 1024 * 1024)
                out["sent_sha256"] = hashlib.sha256(blob).hexdigest()
                write_frame(sock, 2, blob, mask=True)
            elif "ping" in step:
                write_frame(sock, 9, step["ping"].encode(), mask=True)
            elif "fragmented" in step:
                data = step["fragmented"].encode()
                half = len(data) // 2
                write_frame(sock, 1, data[:half], mask=True, fin=False)
                write_frame(sock, 0, data[half:], mask=True, fin=True)
            elif "close" in step:
                write_frame(sock, 8, struct.pack(">H", step["close"]), mask=True)
            elif "sleep" in step:
                time.sleep(step["sleep"])
            elif "recv_timeout" in step:
                sock.settimeout(step["recv_timeout"])
            if step.get("read", "send" in step or "binary_mb" in step or "ping" in step or "fragmented" in step or "close" in step):
                opcode, payload = read_message(sock, mask_replies=True)
                if opcode == 8:
                    out["replies"].append({"close": struct.unpack(">H", payload[:2])[0] if len(payload) >= 2 else None})
                    break
                if opcode == 2 and len(payload) > 4096:
                    out["received_sha256"] = hashlib.sha256(payload).hexdigest()
                    out["replies"].append({"binary": len(payload)})
                else:
                    out["replies"].append({"opcode": opcode, "text": payload.decode("utf-8", "replace")})
    except (socket.timeout, TimeoutError):
        out["ended"] = "client timeout"
    except ConnectionError:
        out["ended"] = "connection closed"
    out["seconds"] = round(time.monotonic() - start, 2)
    return out


def main(argv):
    cmd = argv[1]
    if cmd == "sse-server":
        server = ThreadingHTTPServer(("0.0.0.0", int(argv[2])), SseHandler)
        server.daemon_threads = True
        server.serve_forever()
    elif cmd == "ws-server":
        ThreadingTCP(("0.0.0.0", int(argv[2])), WsHandler).serve_forever()
    elif cmd == "sse-client":
        max_seconds, headers, rest = 20.0, [], argv[3:]
        while rest:
            flag, value, rest = rest[0], rest[1], rest[2:]
            if flag == "--max-seconds":
                max_seconds = float(value)
            elif flag == "--header":
                headers.append(value)
        print(json.dumps(sse_client(argv[2], max_seconds, headers)))
    elif cmd == "ws-client":
        print(json.dumps(ws_client(argv[2], json.loads(argv[3]), os.environ.get("WS_HEADERS", ""))))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
