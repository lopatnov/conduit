//! The messages of Envoy's `envoy.service.ext_proc.v3.ExternalProcessor/Process`, written by hand with prost derives (the
//! build has no `protoc`). Field numbers and types are those of `external_processor.proto`, `config/core/v3/base.proto` and
//! `type/v3/http_status.proto` of Envoy; `tests::*_bytes` pin them with literal wire bytes, so a wrong tag is a test failure,
//! not a processor that silently sees nothing.
//!
//! Only what the implemented phases need is declared. A field this crate does not declare is skipped by prost when a
//! message is decoded (protobuf's rule for unknown fields), so a processor built against the full proto still works.
//! Where the presence of a field matters but its content does not (a body mutation in the answer to the request headers), the
//! field is declared as an empty message ([`Ignored`]).

use prost::Message;

/// The gRPC method of the processor: `POST /envoy.service.ext_proc.v3.ExternalProcessor/Process`.
pub const PROCESS_PATH: &str = "/envoy.service.ext_proc.v3.ExternalProcessor/Process";

/// `config.core.v3.HeaderValue`.
#[derive(Clone, PartialEq, Message)]
pub struct HeaderValue {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
    /// Envoy sends and reads header values as `raw_value` (`value` is the older UTF-8 string form).
    #[prost(bytes = "vec", tag = "3")]
    pub raw_value: Vec<u8>,
}

/// `config.core.v3.HeaderMap`.
#[derive(Clone, PartialEq, Message)]
pub struct HeaderMap {
    #[prost(message, repeated, tag = "1")]
    pub headers: Vec<HeaderValue>,
}

/// `google.protobuf.BoolValue`.
#[derive(Clone, PartialEq, Message)]
pub struct BoolValue {
    #[prost(bool, tag = "1")]
    pub value: bool,
}

/// `config.core.v3.HeaderValueOption.HeaderAppendAction`.
pub mod append_action {
    /// Append to an existing header (a second value) or add it.
    pub const APPEND_IF_EXISTS_OR_ADD: i32 = 0;
    /// Add the header only when it is absent.
    pub const ADD_IF_ABSENT: i32 = 1;
    /// Replace an existing header or add it.
    pub const OVERWRITE_IF_EXISTS_OR_ADD: i32 = 2;
    /// Replace a header only when it exists.
    pub const OVERWRITE_IF_EXISTS: i32 = 3;
}

/// `config.core.v3.HeaderValueOption`.
#[derive(Clone, PartialEq, Message)]
pub struct HeaderValueOption {
    #[prost(message, optional, tag = "1")]
    pub header: Option<HeaderValue>,
    /// Deprecated in Envoy (use `append_action`) but still sent by older processors: `append: false` means overwrite.
    #[prost(message, optional, tag = "2")]
    pub append: Option<BoolValue>,
    #[prost(int32, tag = "3")]
    pub append_action: i32,
    #[prost(bool, tag = "4")]
    pub keep_empty_value: bool,
}

/// `HeaderMutation`.
#[derive(Clone, PartialEq, Message)]
pub struct HeaderMutation {
    #[prost(message, repeated, tag = "1")]
    pub set_headers: Vec<HeaderValueOption>,
    #[prost(string, repeated, tag = "2")]
    pub remove_headers: Vec<String>,
}

/// A message whose content is not read: only its presence is.
#[derive(Clone, PartialEq, Message)]
pub struct Ignored {}

/// `HttpHeaders`: the headers (and pseudo-headers) of the request or the response.
#[derive(Clone, PartialEq, Message)]
pub struct HttpHeaders {
    #[prost(message, optional, tag = "1")]
    pub headers: Option<HeaderMap>,
    /// No body follows.
    #[prost(bool, tag = "3")]
    pub end_of_stream: bool,
}

/// `HttpBody`: a chunk of the request or the response body.
#[derive(Clone, PartialEq, Message)]
pub struct HttpBody {
    #[prost(bytes = "vec", tag = "1")]
    pub body: Vec<u8>,
    /// This is the last chunk.
    #[prost(bool, tag = "2")]
    pub end_of_stream: bool,
}

/// `ProcessingRequest`: what the proxy sends.
#[derive(Clone, PartialEq, Message)]
pub struct ProcessingRequest {
    #[prost(oneof = "processing_request::Request", tags = "2, 3, 4, 5")]
    pub request: Option<processing_request::Request>,
}

/// The `request` oneof of [`ProcessingRequest`]: the headers and the body of the request and of the response (trailers
/// are not sent).
pub mod processing_request {
    /// The oneof.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Request {
        /// `request_headers = 2`.
        #[prost(message, tag = "2")]
        RequestHeaders(super::HttpHeaders),
        /// `response_headers = 3`.
        #[prost(message, tag = "3")]
        ResponseHeaders(super::HttpHeaders),
        /// `request_body = 4`.
        #[prost(message, tag = "4")]
        RequestBody(super::HttpBody),
        /// `response_body = 5`.
        #[prost(message, tag = "5")]
        ResponseBody(super::HttpBody),
    }
}

/// `BodyMutation`: the body to put in place of the chunk that was sent (`oneof mutation { bytes body = 1; bool clear_body
/// = 2; }`).
#[derive(Clone, PartialEq, Message)]
pub struct BodyMutation {
    #[prost(oneof = "body_mutation::Mutation", tags = "1, 2")]
    pub mutation: Option<body_mutation::Mutation>,
}

/// The `mutation` oneof of [`BodyMutation`].
pub mod body_mutation {
    /// The oneof.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Mutation {
        /// `body = 1`: replace the chunk with these bytes.
        #[prost(bytes, tag = "1")]
        Body(Vec<u8>),
        /// `clear_body = 2`: drop the chunk.
        #[prost(bool, tag = "2")]
        ClearBody(bool),
    }
}

/// `CommonResponse` (the `response` of a `HeadersResponse` or `BodyResponse`).
#[derive(Clone, PartialEq, Message)]
pub struct CommonResponse {
    /// `ResponseStatus`: 0 `CONTINUE`, 1 `CONTINUE_AND_REPLACE`.
    #[prost(int32, tag = "1")]
    pub status: i32,
    #[prost(message, optional, tag = "2")]
    pub header_mutation: Option<HeaderMutation>,
    /// Applied in the answer to a body chunk; ignored (and logged) in the answer to headers.
    #[prost(message, optional, tag = "3")]
    pub body_mutation: Option<BodyMutation>,
}

/// `HeadersResponse` and `BodyResponse` (the same shape).
#[derive(Clone, PartialEq, Message)]
pub struct PhaseResponse {
    #[prost(message, optional, tag = "1")]
    pub response: Option<CommonResponse>,
}

/// `envoy.type.v3.HttpStatus`.
#[derive(Clone, PartialEq, Message)]
pub struct HttpStatus {
    /// The HTTP status code (the enum's numbers are the codes).
    #[prost(int32, tag = "1")]
    pub code: i32,
}

/// `ImmediateResponse`: answer the client now and stop.
#[derive(Clone, PartialEq, Message)]
pub struct ImmediateResponse {
    #[prost(message, optional, tag = "1")]
    pub status: Option<HttpStatus>,
    #[prost(message, optional, tag = "2")]
    pub headers: Option<HeaderMutation>,
    #[prost(bytes = "vec", tag = "3")]
    pub body: Vec<u8>,
    /// Not used by this proxy (it is for gRPC clients).
    #[prost(message, optional, tag = "4")]
    pub grpc_status: Option<Ignored>,
    /// A reason for the logs.
    #[prost(string, tag = "5")]
    pub details: String,
}

/// `ProcessingResponse`: what the processor sends.
#[derive(Clone, PartialEq, Message)]
pub struct ProcessingResponse {
    #[prost(oneof = "processing_response::Response", tags = "1, 2, 3, 4, 5, 6, 7")]
    pub response: Option<processing_response::Response>,
}

/// The `response` oneof of [`ProcessingResponse`]. Every arm is declared, so an answer for a phase this proxy has not
/// asked about is recognised and reported, not mistaken for an empty message.
pub mod processing_response {
    /// The oneof.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Response {
        /// `request_headers = 1`.
        #[prost(message, tag = "1")]
        RequestHeaders(super::PhaseResponse),
        /// `response_headers = 2`.
        #[prost(message, tag = "2")]
        ResponseHeaders(super::PhaseResponse),
        /// `request_body = 3`.
        #[prost(message, tag = "3")]
        RequestBody(super::PhaseResponse),
        /// `response_body = 4`.
        #[prost(message, tag = "4")]
        ResponseBody(super::PhaseResponse),
        /// `request_trailers = 5`.
        #[prost(message, tag = "5")]
        RequestTrailers(super::Ignored),
        /// `response_trailers = 6`.
        #[prost(message, tag = "6")]
        ResponseTrailers(super::Ignored),
        /// `immediate_response = 7`.
        #[prost(message, tag = "7")]
        ImmediateResponse(super::ImmediateResponse),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Request headers `{ "k": "v" }`, end of stream:
    /// `ProcessingRequest.request_headers (2)` > `HttpHeaders.headers (1)` > `HeaderMap.headers (1)` >
    /// `HeaderValue { key (1) "k", raw_value (3) "v" }`, then `HttpHeaders.end_of_stream (3)`.
    #[test]
    fn a_request_headers_message_has_the_envoy_wire_bytes() {
        let msg = ProcessingRequest {
            request: Some(processing_request::Request::RequestHeaders(HttpHeaders {
                headers: Some(HeaderMap {
                    headers: vec![HeaderValue {
                        key: "k".into(),
                        value: String::new(),
                        raw_value: b"v".to_vec(),
                    }],
                }),
                end_of_stream: true,
            })),
        };
        let expected: &[u8] = &[
            0x12, 0x0c, // request_headers (2), 12 bytes
            0x0a, 0x08, //   headers (1), 8 bytes
            0x0a, 0x06, //     headers (1), 6 bytes
            0x0a, 0x01, b'k', //  key (1)
            0x1a, 0x01, b'v', //  raw_value (3)
            0x18, 0x01, //   end_of_stream (3)
        ];
        assert_eq!(msg.encode_to_vec(), expected);
    }

    /// A processor's answer: `request_headers (1)` > `response (1)` > `header_mutation (2)` > `set_headers (1)` >
    /// `HeaderValueOption { header (1) { key "a", raw_value "b" }, append_action (3) = 2 }` and `remove_headers (2) "r"`.
    #[test]
    fn a_header_mutation_answer_decodes_from_the_envoy_wire_bytes() {
        let bytes: &[u8] = &[
            0x0a, 0x13, // request_headers (1), 19 bytes
            0x0a, 0x11, //   response (1), 17 bytes
            0x12, 0x0f, //     header_mutation (2), 15 bytes
            0x0a, 0x0a, //       set_headers (1), 10 bytes
            0x0a, 0x06, //         header (1), 6 bytes
            0x0a, 0x01, b'a', //     key
            0x1a, 0x01, b'b', //     raw_value
            0x18, 0x02, //         append_action (3) = OVERWRITE_IF_EXISTS_OR_ADD
            0x12, 0x01, b'r', //     remove_headers (2)
        ];
        let msg = ProcessingResponse::decode(bytes).expect("decodes");
        let Some(processing_response::Response::RequestHeaders(phase)) = msg.response else {
            panic!("not a request_headers answer: {msg:?}");
        };
        let mutation = phase.response.unwrap().header_mutation.unwrap();
        assert_eq!(mutation.remove_headers, ["r"]);
        let set = &mutation.set_headers[0];
        assert_eq!(set.header.as_ref().unwrap().key, "a");
        assert_eq!(set.header.as_ref().unwrap().raw_value, b"b");
        assert_eq!(set.append_action, append_action::OVERWRITE_IF_EXISTS_OR_ADD);
    }

    /// `immediate_response (7)` > `status (1)` > `code (1)` = 403, `body (3)` "no", `details (5)` "why".
    #[test]
    fn an_immediate_response_decodes_from_the_envoy_wire_bytes() {
        let bytes: &[u8] = &[
            0x3a, 0x0e, // immediate_response (7), 14 bytes
            0x0a, 0x03, //   status (1), 3 bytes
            0x08, 0x93, 0x03, //   code (1) = 403 (varint 0x193)
            0x1a, 0x02, b'n', b'o', //   body (3)
            0x2a, 0x03, b'w', b'h', b'y', //   details (5)
        ];
        let msg = ProcessingResponse::decode(bytes).expect("decodes");
        let Some(processing_response::Response::ImmediateResponse(r)) = msg.response else {
            panic!("not an immediate response: {msg:?}");
        };
        assert_eq!(r.status.unwrap().code, 403);
        assert_eq!(r.body, b"no");
        assert_eq!(r.details, "why");
    }

    /// `request_body (4)` > `HttpBody { body (1) "ab", end_of_stream (2) }`; the response phases use tags 3 (headers) and 5 (body).
    #[test]
    fn body_and_response_messages_have_the_envoy_wire_bytes() {
        let body = HttpBody {
            body: b"ab".to_vec(),
            end_of_stream: true,
        };
        let request_body = ProcessingRequest {
            request: Some(processing_request::Request::RequestBody(body.clone())),
        };
        assert_eq!(
            request_body.encode_to_vec(),
            [0x22, 0x06, 0x0a, 0x02, b'a', b'b', 0x10, 0x01]
        );
        let response_body = ProcessingRequest {
            request: Some(processing_request::Request::ResponseBody(body)),
        };
        assert_eq!(
            response_body.encode_to_vec(),
            [0x2a, 0x06, 0x0a, 0x02, b'a', b'b', 0x10, 0x01]
        );
        let response_headers = ProcessingRequest {
            request: Some(processing_request::Request::ResponseHeaders(HttpHeaders {
                headers: Some(HeaderMap { headers: vec![] }),
                end_of_stream: true,
            })),
        };
        // response_headers (3) > headers (1, empty), end_of_stream (3)
        assert_eq!(
            response_headers.encode_to_vec(),
            [0x1a, 0x04, 0x0a, 0x00, 0x18, 0x01]
        );
    }

    /// `request_body (3)` > `response (1)` > `body_mutation (3)` > `body (1) "xy"` or `clear_body (2)`.
    #[test]
    fn a_body_mutation_decodes_from_the_envoy_wire_bytes() {
        let replace: &[u8] = &[0x1a, 0x08, 0x0a, 0x06, 0x1a, 0x04, 0x0a, 0x02, b'x', b'y'];
        let msg = ProcessingResponse::decode(replace).expect("decodes");
        let Some(processing_response::Response::RequestBody(phase)) = msg.response else {
            panic!("not a request_body answer: {msg:?}");
        };
        let mutation = phase.response.unwrap().body_mutation.unwrap().mutation;
        assert_eq!(
            mutation,
            Some(body_mutation::Mutation::Body(b"xy".to_vec()))
        );

        let clear: &[u8] = &[0x1a, 0x06, 0x0a, 0x04, 0x1a, 0x02, 0x10, 0x01];
        let msg = ProcessingResponse::decode(clear).expect("decodes");
        let Some(processing_response::Response::RequestBody(phase)) = msg.response else {
            panic!("not a request_body answer: {msg:?}");
        };
        let mutation = phase.response.unwrap().body_mutation.unwrap().mutation;
        assert_eq!(mutation, Some(body_mutation::Mutation::ClearBody(true)));
    }

    /// Fields this crate does not declare (`mode_override (9)`, `dynamic_metadata (8)`) are skipped, not an error.
    #[test]
    fn undeclared_fields_of_an_answer_are_skipped() {
        let bytes: &[u8] = &[
            0x0a, 0x00, // request_headers (1), empty
            0x4a, 0x02, 0x08, 0x01, // mode_override (9), 2 bytes of something
            0x42, 0x00, // dynamic_metadata (8), empty
        ];
        let msg = ProcessingResponse::decode(bytes).expect("decodes");
        assert!(matches!(
            msg.response,
            Some(processing_response::Response::RequestHeaders(_))
        ));
    }
}
