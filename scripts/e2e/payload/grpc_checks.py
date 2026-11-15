#!/usr/bin/env python3
"""The checks of the `grpc` e2e scenario: gRPC calls through Conduit.

    grpc_checks.py MODE TARGET [--ca CA]       MODE: works | refused

`works`: every call of grpc_echo.client must come out right (unary with trailing metadata, server streaming that arrives
message by message, client streaming of 1 MB, an error status with its message and trailing metadata, a deadline).
`refused`: the same calls must all fail with UNAVAILABLE, which is what a cleartext gRPC upstream answers when the route
says `http2: true` and not `http2: "h2c"` (Conduit then speaks HTTP/1.1 to it). Prints `ok: ...` or `FAIL: ...` (exit 1).
"""
import sys

import grpc_echo


def fail(message):
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def main():
    mode, target = sys.argv[1:3]
    ca = sys.argv[sys.argv.index("--ca") + 1] if "--ca" in sys.argv else None
    out = grpc_echo.client(target, ca, None)
    if mode == "refused":
        for name, result in out.items():
            if not (isinstance(result, dict) and result.get("status") == "UNAVAILABLE"):
                fail(f"{name} through a route with http2: true to a cleartext gRPC server answered {result}")
        print("ok: http2: true to a cleartext gRPC upstream is refused (HTTP/1.1 is spoken to it): every call is UNAVAILABLE")
        return

    unary = out["unary"]
    if unary.get("reply") != "ping" or unary.get("status") != "OK":
        fail(f"unary {unary}")
    if not unary["trailers"].get("x-seen-ua", "").startswith("grpc-python"):
        fail(f"the upstream did not see the client's user-agent: {unary}")
    streaming = out["server_streaming"]
    arrival = streaming.get("arrival_seconds", [])
    if streaming.get("items") != ["item 0", "item 1", "item 2"] or len(arrival) != 3 or arrival[2] - arrival[0] < 0.3:
        fail(f"server streaming was not delivered message by message: {streaming}")
    if out["client_streaming"] != {"total": "1000000"}:
        fail(f"client streaming of 1 MB: {out['client_streaming']}")
    failed = out["fail"]
    if (failed.get("status"), failed.get("details")) != ("INVALID_ARGUMENT", "bad request: oops") \
            or failed["trailers"].get("x-detail") != "kept":
        fail(f"an error status lost its message or its trailing metadata: {failed}")
    if out["deadline"].get("status") != "DEADLINE_EXCEEDED":
        fail(f"deadline: {out['deadline']}")
    print("ok: unary with trailing metadata, server streaming (message by message), client streaming (1 MB), "
          "an error status with message and trailers, a deadline")


if __name__ == "__main__":
    main()
