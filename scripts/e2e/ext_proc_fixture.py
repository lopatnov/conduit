#!/usr/bin/env python3
"""The two processes of the ext_proc scenario (scripts/e2e/ext-proc.sh): an upstream and an external processor.

    ext_proc_fixture.py upstream  --port N
    ext_proc_fixture.py processor --port N --observed FILE

The processor is the point of the scenario. It is written against the messages that Envoy generates from its own
`external_processor.proto` (the PyPI package `xds-protos`, which is `envoy/service/ext_proc/v3/*.proto` compiled by
protoc), served by grpcio. Nothing of Conduit's hand-written messages is shared with it, so a wrong field number, a wrong
framing or a wrong reading of the protocol in Conduit cannot be hidden by the same mistake on this side.

What the processor does (each rule belongs to one phase, so one request exercises the ones its entry asks for):
  request headers   adds `x-processed-by`, removes `x-remove-me`; answers `/deny*` with an immediate response 403
  request body      replaces `secret` by `******`; answers a body that contains `attack` with an immediate response 403
  response headers  adds `x-processor-saw-status` (the `:status` it was shown), removes `x-upstream-secret`;
                    replaces a 500 by an immediate response 503
  response body     appends what it was shown to the file given by --observed (no answer is sent)
"""
import argparse
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


# ── upstream ────────────────────────────────────────────────────────────────────────────────────────────────────────


class Upstream(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def _read_body(self):
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            body = b""
            while True:
                size = int(self.rfile.readline().strip() or b"0", 16)
                chunk = self.rfile.read(size)
                self.rfile.readline()
                if size == 0:
                    return body
                body += chunk
        return self.rfile.read(int(self.headers.get("Content-Length") or 0))

    def _send(self, status, payload, extra=()):
        self.send_response(status)
        self.send_header("Content-Type", "text/plain")
        for name, value in extra:
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _handle(self):
        body = self._read_body()
        if self.path == "/static":
            self._send(200, b"upstream says hi", [("X-Upstream-Secret", "s3"), ("X-Keep", "1")])
        elif self.path == "/fail":
            self._send(500, b"boom")
        else:
            echo = {
                "method": self.command,
                "path": self.path,
                "headers": {k.lower(): v for k, v in self.headers.items()},
                "body": body.decode("utf-8", "replace"),
            }
            self._send(200, json.dumps(echo).encode())

    do_GET = do_POST = do_PUT = _handle


def run_upstream(port):
    ThreadingHTTPServer(("127.0.0.1", port), Upstream).serve_forever()


# ── processor ───────────────────────────────────────────────────────────────────────────────────────────────────────


def run_processor(port, observed):
    from concurrent import futures

    import grpc
    from envoy.config.core.v3 import base_pb2
    from envoy.service.ext_proc.v3 import external_processor_pb2 as pb
    from envoy.service.ext_proc.v3 import external_processor_pb2_grpc as services
    from envoy.type.v3 import http_status_pb2

    lock = threading.Lock()

    def header(key, value):
        return base_pb2.HeaderValueOption(
            header=base_pb2.HeaderValue(key=key, raw_value=value.encode()),
            append_action=base_pb2.HeaderValueOption.OVERWRITE_IF_EXISTS_OR_ADD,
        )

    def immediate(code, body):
        return pb.ProcessingResponse(
            immediate_response=pb.ImmediateResponse(
                status=http_status_pb2.HttpStatus(code=code),
                headers=pb.HeaderMutation(set_headers=[header("x-by", "envoy-proto-processor")]),
                body=body.encode(),
                details="e2e",
            )
        )

    def headers_of(message):
        return {h.key: (h.raw_value or h.value.encode()).decode() for h in message.headers.headers}

    class Processor(services.ExternalProcessorServicer):
        def Process(self, request_iterator, context):
            for request in request_iterator:
                kind = request.WhichOneof("request")
                if kind == "request_headers":
                    headers = headers_of(request.request_headers)
                    if headers.get(":path", "").startswith("/deny"):
                        yield immediate(403, "denied by the processor\n")
                        return
                    yield pb.ProcessingResponse(
                        request_headers=pb.HeadersResponse(
                            response=pb.CommonResponse(
                                header_mutation=pb.HeaderMutation(
                                    set_headers=[header("x-processed-by", "envoy-proto-processor")],
                                    remove_headers=["x-remove-me"],
                                )
                            )
                        )
                    )
                elif kind == "request_body":
                    text = request.request_body.body.decode("utf-8", "replace")
                    if "attack" in text:
                        yield immediate(403, "blocked by the processor\n")
                        return
                    yield pb.ProcessingResponse(
                        request_body=pb.BodyResponse(
                            response=pb.CommonResponse(
                                body_mutation=pb.BodyMutation(body=text.replace("secret", "******").encode())
                            )
                        )
                    )
                elif kind == "response_headers":
                    headers = headers_of(request.response_headers)
                    status = headers.get(":status", "")
                    if status == "500":
                        yield immediate(503, '{"error":"masked by the processor"}')
                        return
                    yield pb.ProcessingResponse(
                        response_headers=pb.HeadersResponse(
                            response=pb.CommonResponse(
                                header_mutation=pb.HeaderMutation(
                                    set_headers=[header("x-processor-saw-status", status)],
                                    remove_headers=["x-upstream-secret"],
                                )
                            )
                        )
                    )
                elif kind == "response_body":
                    with lock, open(observed, "ab") as out:
                        out.write(request.response_body.body)
                    # an observed body is not answered

    server = grpc.server(futures.ThreadPoolExecutor(max_workers=8))
    services.add_ExternalProcessorServicer_to_server(Processor(), server)
    server.add_insecure_port(f"127.0.0.1:{port}")
    server.start()
    print(f"processor listening on 127.0.0.1:{port}", flush=True)
    server.wait_for_termination()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("role", choices=["upstream", "processor"])
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--observed", default="/dev/null")
    args = parser.parse_args()
    if args.role == "upstream":
        run_upstream(args.port)
    else:
        run_processor(args.port, args.observed)


if __name__ == "__main__":
    sys.exit(main())
