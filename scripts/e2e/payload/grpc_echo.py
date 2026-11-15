#!/usr/bin/env python3
"""A gRPC server and client for the e2e scenario, without generated code: messages are raw bytes.

    grpc_echo.py server PORT [--tls CERT KEY]
    grpc_echo.py client TARGET [--ca CA] [--authority NAME]

The service `e2e.Echo` has `Unary` (echoes the bytes, with trailing metadata `x-seen-te` and `x-seen-ua` taken from the
request), `Count` (server streaming: the request is a decimal number N, the reply is N messages), `Collect` (client
streaming: replies with the total length of what it got), `Fail` (ends with status INVALID_ARGUMENT and a message) and
`Slow` (answers after the number of seconds in the request). `client` runs every call and prints one JSON object (`grpc_checks.py` imports it and checks the answers).
Needs the `grpcio` package (Debian: python3-grpcio).
"""
import json
import os
import socket
import sys
import time
from concurrent import futures

import grpc

IDENTITY = {"request_deserializer": lambda b: b, "response_serializer": lambda b: b}


def unary(request, context):
    md = dict(context.invocation_metadata())
    context.set_trailing_metadata((("x-seen-te", md.get("te", "")), ("x-seen-ua", md.get("user-agent", "")[:20])))
    return request


def count(request, context):
    for i in range(int(request)):
        yield f"item {i}".encode()
        time.sleep(0.2)


def collect(request_iterator, context):
    return str(sum(len(m) for m in request_iterator)).encode()


def fail(request, context):
    context.set_trailing_metadata((("x-detail", "kept"),))
    context.abort(grpc.StatusCode.INVALID_ARGUMENT, "bad request: " + request.decode())


def slow(request, context):
    time.sleep(float(request))
    return b"done"


def bind_address():
    """The address of this container: the server listens there and not on every interface (BIND_ADDRESS overrides)."""
    return os.environ.get("BIND_ADDRESS") or socket.gethostbyname(socket.gethostname())


def serve(port, tls):
    handlers = {
        "Unary": grpc.unary_unary_rpc_method_handler(unary, **IDENTITY),
        "Count": grpc.unary_stream_rpc_method_handler(count, **IDENTITY),
        "Collect": grpc.stream_unary_rpc_method_handler(collect, **IDENTITY),
        "Fail": grpc.unary_unary_rpc_method_handler(fail, **IDENTITY),
        "Slow": grpc.unary_unary_rpc_method_handler(slow, **IDENTITY),
    }
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=16))
    server.add_generic_rpc_handlers((grpc.method_handlers_generic_handler("e2e.Echo", handlers),))
    if tls:
        creds = grpc.ssl_server_credentials([(open(tls[1], "rb").read(), open(tls[0], "rb").read())])
        server.add_secure_port(f"{bind_address()}:{port}", creds)
    else:
        server.add_insecure_port(f"{bind_address()}:{port}")
    server.start()
    server.wait_for_termination()


def client(target, ca, authority):
    options = [("grpc.keepalive_time_ms", 600000)]
    if authority:
        options.append(("grpc.default_authority", authority))
    if ca:
        channel = grpc.secure_channel(target, grpc.ssl_channel_credentials(open(ca, "rb").read()), options)
    else:
        channel = grpc.insecure_channel(target, options)
    ident = {"request_serializer": lambda b: b, "response_deserializer": lambda b: b}
    out = {}

    def call(name, fn):
        try:
            out[name] = fn()
        except grpc.RpcError as e:
            out[name] = {"status": e.code().name, "details": e.details(),
                         "trailers": {k: v for k, v in (e.trailing_metadata() or ())}}

    def unary_call():
        reply, call = channel.unary_unary("/e2e.Echo/Unary", **ident).with_call(b"ping")
        return {"reply": reply.decode(), "status": call.code().name,
                "trailers": {k: v for k, v in call.trailing_metadata()}}

    def streaming():
        started = time.monotonic()
        stamps = []
        items = []
        for message in channel.unary_stream("/e2e.Echo/Count", **ident)(b"3"):
            items.append(message.decode())
            stamps.append(round(time.monotonic() - started, 2))
        return {"items": items, "arrival_seconds": stamps}

    def collect_call():
        parts = (b"x" * 100000 for _ in range(10))
        return {"total": channel.stream_unary("/e2e.Echo/Collect", **ident)(parts).decode()}

    call("unary", unary_call)
    call("server_streaming", streaming)
    call("client_streaming", collect_call)
    call("fail", lambda: channel.unary_unary("/e2e.Echo/Fail", **ident)(b"oops"))
    call("deadline", lambda: channel.unary_unary("/e2e.Echo/Slow", **ident)(b"3", timeout=1))
    return out


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[0] == "server":
        serve(int(args[1]), tuple(args[3:5]) if "--tls" in args else None)
    elif args[0] == "client":
        ca = args[args.index("--ca") + 1] if "--ca" in args else None
        authority = args[args.index("--authority") + 1] if "--authority" in args else None
        print(json.dumps(client(args[1], ca, authority)))
    else:
        sys.exit(__doc__)
