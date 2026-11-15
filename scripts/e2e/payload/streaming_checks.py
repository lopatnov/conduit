#!/usr/bin/env python3
"""The checks of the `streaming` e2e scenario: Server-Sent Events and WebSocket through Conduit.

    streaming_checks.py HTTP_BASE HTTPS_BASE      e.g. http://app.test:8080 https://app.test:8443

Needs the routes of scripts/e2e/streaming.sh and a CA file in $CA_FILE for the https base. Prints `ok: ...` for every
check that holds and `FAIL: ...` (exit 1) for the first that does not. Every number below is a margin around a delay
that the test servers (streaming.py) create on purpose, not a benchmark.
"""
import json
import sys

import streaming


def fail(message):
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def ok(message):
    print(f"ok: {message}")


def check(condition, message):
    if not condition:
        fail(message)


def ws_url(base, path):
    return base.replace("http", "ws", 1) + path


def sse_checks(base, label):
    # 1. Events reach the client as the upstream sends them, not when the stream ends. The server sends three events
    #    0.5 s apart; the compression of the site is on and the client offers gzip, and a stream is not compressed
    #    (a compressor would hold events back until its buffer fills).
    out = streaming.sse_client(base + "/sse/events?n=3&gap=0.5", 20, ["Accept-Encoding: gzip, br"])
    check(out["status"] == 200, f"{label}: SSE status {out}")
    check(out["content_type"].startswith("text/event-stream"), f"{label}: content type {out['content_type']}")
    check(out["content_encoding"] is None, f"{label}: an event stream was compressed: {out['content_encoding']}")
    at = [e["at"] for e in out["events"]]
    check([e["data"] for e in out["events"]] == ["tick 0", "tick 1", "tick 2"], f"{label}: events {out['events']}")
    check(at[0] < 0.4 and at[2] - at[0] >= 0.8, f"{label}: events were not delivered as they were sent: {at}")
    ok(f"{label}: SSE events arrive one by one, uncompressed, content type kept")

    # 2. A reconnecting EventSource sends Last-Event-ID; the upstream must see it.
    out = streaming.sse_client(base + "/sse/echo-header", 10, ["Last-Event-ID: 42"])
    seen = json.loads(out["events"][0]["data"])
    check(seen["last-event-id"] == "42" and seen["accept"] == "text/event-stream", f"{label}: upstream saw {seen}")
    ok(f"{label}: Last-Event-ID and Accept reach the upstream")

    # 3. `timeout.readMs` is an idle timeout for a stream: the upstream is silent for 4 s between two events.
    out = streaming.sse_client(base + "/quick-sse/quiet?gap=4", 20)
    check([e["data"] for e in out["events"]] == ["first"] and 1.5 <= out["seconds"] < 3.5,
          f"{label}: readMs 2000 did not end an idle stream after about 2 s: {out}")
    out = streaming.sse_client(base + "/patient-sse/quiet?gap=4", 20)
    check([e["data"] for e in out["events"]] == ["first", "second"] and out["seconds"] >= 3.8,
          f"{label}: readMs 10000 cut a stream that was idle for 4 s: {out}")
    ok(f"{label}: an idle stream ends after timeout.readMs and survives a silence shorter than it")


def ws_checks(base, label, https):
    url = lambda path: ws_url(base, path)
    one_mb = streaming.ws_client(url("/ws"), [
        {"send": "hello"}, {"ping": "keepalive"}, {"fragmented": "split in two frames"}, {"binary_mb": 1}, {"close": 1000},
    ])
    check(one_mb["status"] == 101 and one_mb["accept_ok"], f"{label}: upgrade failed: {one_mb}")
    replies = one_mb["replies"]
    check(replies[0] == {"opcode": 1, "text": "hello"}, f"{label}: text echo {replies}")
    check(replies[1] == {"opcode": 10, "text": "keepalive"}, f"{label}: pong {replies}")
    check(replies[2] == {"opcode": 1, "text": "split in two frames"}, f"{label}: fragmented message {replies}")
    check(replies[3] == {"binary": 1048576} and one_mb["sent_sha256"] == one_mb["received_sha256"],
          f"{label}: 1 MiB binary message changed on the way: {replies[3:4]}")
    check(replies[4] == {"close": 1000}, f"{label}: close handshake {replies}")
    ok(f"{label}: upgrade, text, ping/pong, fragments, 1 MiB binary and the close handshake go through")

    out = streaming.ws_client(url("/ws"), [{"send": "seen?"}, {"send": "close:4001"}],
                              extra_headers="Origin: https://shop.example\r\n")
    seen = json.loads(out["replies"][0]["text"].split(":", 1)[1])
    check(seen["path"] == "/" and seen["origin"] == "https://shop.example" and seen["x-forwarded-for"],
          f"{label}: upstream saw {seen}")
    check(out["replies"][1] == {"close": 4001}, f"{label}: a close from the upstream side: {out['replies']}")
    ok(f"{label}: the upstream sees the stripped path, the Origin and X-Forwarded-For; its close code reaches the client")

    out = streaming.ws_client(url("/plain-ws"), [{"send": "hi"}])
    check(out["status"] == 502, f"{label}: an upgrade on a route without `websocket: true` answered {out['status']}")
    ok(f"{label}: without `websocket: true` the upgrade is refused with 502")

    # idle: `timeout.readMs` also ends a WebSocket in which nothing is sent either way; pings keep it open.
    out = streaming.ws_client(url("/quick-ws"), [{"recv_timeout": 15}, {"sleep": 4}, {"send": "late"}])
    check(out.get("ended") == "connection closed" and not out["replies"], f"{label}: an idle socket stayed open: {out}")
    pings = [{"ping": str(i)} for i in range(1, 5)]
    steps = [{"recv_timeout": 15}]
    for p in pings:
        steps += [p, {"sleep": 1}]
    steps += [{"send": "still here"}]
    out = streaming.ws_client(url("/quick-ws"), steps)
    check(out["replies"][-1] == {"opcode": 1, "text": "still here"} and out["seconds"] >= 3.5,
          f"{label}: pings did not keep the socket open: {out}")
    out = streaming.ws_client(url("/patient-ws"), [{"recv_timeout": 15}, {"send": "sleep:4"}])
    check(out["replies"][-1] == {"opcode": 1, "text": "slept"}, f"{label}: readMs 10000 cut a 4 s wait: {out}")
    ok(f"{label}: an idle WebSocket ends after timeout.readMs; pings, or a larger readMs, keep it")


def main():
    http_base, https_base = sys.argv[1:3]
    sse_checks(http_base, "http")
    sse_checks(https_base, "https")
    ws_checks(http_base, "ws", False)
    ws_checks(https_base, "wss", True)


if __name__ == "__main__":
    main()
