//! The messages that tell the processor about a request, a response and their bodies (what the proxy sends), as Envoy's
//! `ProcessingRequest`. The answers are read in [`crate::client`].

use crate::client::{RequestInfo, ResponseInfo};
use crate::proto::{
    self, processing_request, HeaderMap as PbHeaderMap, HttpHeaders, ProcessingRequest,
};

/// The message that tells the processor about the request.
pub(crate) fn request_message(info: &RequestInfo<'_>) -> ProcessingRequest {
    let mut headers = Vec::with_capacity(4 + info.headers.len());
    for (key, value) in [
        (":method", info.method),
        (":scheme", info.scheme),
        (":authority", info.authority),
        (":path", info.path),
    ] {
        headers.push(proto::HeaderValue {
            key: key.to_owned(),
            value: String::new(),
            raw_value: value.as_bytes().to_vec(),
        });
    }
    for (name, value) in info.headers {
        headers.push(proto::HeaderValue {
            key: name.as_str().to_owned(),
            value: String::new(),
            raw_value: value.as_bytes().to_vec(),
        });
    }
    ProcessingRequest {
        request: Some(processing_request::Request::RequestHeaders(HttpHeaders {
            headers: Some(PbHeaderMap { headers }),
            end_of_stream: info.end_of_stream,
        })),
    }
}

/// The message that tells the processor about the response.
pub(crate) fn response_message(info: &ResponseInfo<'_>) -> ProcessingRequest {
    let mut headers = Vec::with_capacity(1 + info.headers.len());
    headers.push(proto::HeaderValue {
        key: ":status".to_owned(),
        value: String::new(),
        raw_value: info.status.to_string().into_bytes(),
    });
    for (name, value) in info.headers {
        headers.push(proto::HeaderValue {
            key: name.as_str().to_owned(),
            value: String::new(),
            raw_value: value.as_bytes().to_vec(),
        });
    }
    ProcessingRequest {
        request: Some(processing_request::Request::ResponseHeaders(HttpHeaders {
            headers: Some(PbHeaderMap { headers }),
            end_of_stream: info.end_of_stream,
        })),
    }
}

/// The message that carries a chunk of the request (`request`) or response body.
pub(crate) fn body_message(chunk: &[u8], end_of_stream: bool, request: bool) -> ProcessingRequest {
    let body = proto::HttpBody {
        body: chunk.to_vec(),
        end_of_stream,
    };
    ProcessingRequest {
        request: Some(if request {
            processing_request::Request::RequestBody(body)
        } else {
            processing_request::Request::ResponseBody(body)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderMap, HeaderValue};

    #[test]
    fn the_processor_is_told_the_pseudo_headers_and_every_header_value() {
        let mut headers = HeaderMap::new();
        headers.append("x-a", HeaderValue::from_static("1"));
        headers.append("x-a", HeaderValue::from_static("2"));
        headers.insert("cookie", HeaderValue::from_bytes(b"k=\xff").unwrap());
        let message = request_message(&RequestInfo {
            method: "POST",
            scheme: "https",
            authority: "example.com",
            path: "/p?q=1",
            headers: &headers,
            end_of_stream: false,
        });
        let Some(processing_request::Request::RequestHeaders(h)) = message.request else {
            panic!("not request headers");
        };
        assert!(!h.end_of_stream);
        let sent: Vec<(String, Vec<u8>)> = h
            .headers
            .unwrap()
            .headers
            .into_iter()
            .map(|v| (v.key, v.raw_value))
            .collect();
        assert_eq!(
            sent,
            [
                (":method".to_owned(), b"POST".to_vec()),
                (":scheme".to_owned(), b"https".to_vec()),
                (":authority".to_owned(), b"example.com".to_vec()),
                (":path".to_owned(), b"/p?q=1".to_vec()),
                ("x-a".to_owned(), b"1".to_vec()),
                ("x-a".to_owned(), b"2".to_vec()),
                ("cookie".to_owned(), b"k=\xff".to_vec()),
            ]
        );
    }
}
