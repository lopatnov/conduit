#!/usr/bin/env python3
"""A small external processor for Conduit's `ext_proc` filter (see docs/ext-proc.md).

    pip install grpcio==1.85.1 xds-protos==1.85.1 protobuf==7.36.2 typing_extensions==4.16.0
    python3 processor.py 50051

What it does:
  * request headers  - answers `/admin...` with an immediate 403, otherwise adds `x-checked-by: processor.py`
                       and removes `x-internal`
  * request body     - (with `requestBody: buffered`) refuses a body that contains `DROP TABLE`
  * response headers - (with `responsePhase: true`) removes `server` from the answer
"""
import sys
from concurrent import futures

import grpc
from envoy.config.core.v3 import base_pb2
from envoy.service.ext_proc.v3 import external_processor_pb2 as pb
from envoy.service.ext_proc.v3 import external_processor_pb2_grpc as services
from envoy.type.v3 import http_status_pb2


def set_header(key, value):
    return base_pb2.HeaderValueOption(
        header=base_pb2.HeaderValue(key=key, raw_value=value.encode()),
        append_action=base_pb2.HeaderValueOption.OVERWRITE_IF_EXISTS_OR_ADD,
    )


def refuse(code, text):
    return pb.ProcessingResponse(
        immediate_response=pb.ImmediateResponse(
            status=http_status_pb2.HttpStatus(code=code), body=text.encode()
        )
    )


def header_value(message, name):
    for h in message.headers.headers:
        if h.key == name:
            return (h.raw_value or h.value.encode()).decode()
    return ""


class Processor(services.ExternalProcessorServicer):
    def Process(self, request_iterator, context):
        # One stream per request: the messages arrive in order, each is answered once.
        for request in request_iterator:
            kind = request.WhichOneof("request")
            if kind == "request_headers":
                # a demonstration: real path rules must normalise the path first (//admin, %2e, dot segments)
                if header_value(request.request_headers, ":path").startswith("/admin"):
                    yield refuse(403, "admin is not for you\n")
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
            elif kind == "request_body":
                if b"DROP TABLE" in request.request_body.body:
                    yield refuse(400, "that body is refused\n")
                    return
                yield pb.ProcessingResponse(request_body=pb.BodyResponse())  # keep the body as it is
            elif kind == "response_headers":
                yield pb.ProcessingResponse(
                    response_headers=pb.HeadersResponse(
                        response=pb.CommonResponse(
                            header_mutation=pb.HeaderMutation(remove_headers=["server"])
                        )
                    )
                )


def main(port):
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=8))
    services.add_ExternalProcessorServicer_to_server(Processor(), server)
    server.add_insecure_port(f"127.0.0.1:{port}")
    server.start()
    print(f"processor listening on 127.0.0.1:{port}", flush=True)
    server.wait_for_termination()


if __name__ == "__main__":
    main(int(sys.argv[1]) if len(sys.argv) > 1 else 50051)
