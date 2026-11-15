// Tests that proxy real traffic are compiled out without the `proxy` feature, which leaves their helpers unused there.
#![cfg_attr(not(feature = "proxy"), allow(dead_code))]
//! A whole Conduit in front of an in-process `ext_proc` gRPC processor (issue #27, slice 1): the request headers go to
//! the processor, its header mutations reach the upstream, its immediate response reaches the client, and a processor
//! that fails is handled by the entry's policy (fail-closed by default).

mod common;
#[path = "../crates/conduit-ext-proc/tests/support/mod.rs"]
mod support;

use conduit_ext_proc::proto::{
    append_action, body_mutation, processing_request, processing_response, BodyMutation,
    CommonResponse, HeaderMutation, HeaderValue, HeaderValueOption, HttpStatus, ImmediateResponse,
    PhaseResponse, ProcessingRequest, ProcessingResponse,
};
use support::{script, Behaviour, Reply};

fn option(name: &str, value: &str, action: i32) -> HeaderValueOption {
    HeaderValueOption {
        header: Some(HeaderValue {
            key: name.into(),
            value: String::new(),
            raw_value: value.as_bytes().to_vec(),
        }),
        append: None,
        append_action: action,
        keep_empty_value: false,
    }
}

fn mutation(mutation: HeaderMutation) -> Behaviour {
    Behaviour::Answer(ProcessingResponse {
        response: Some(processing_response::Response::RequestHeaders(
            PhaseResponse {
                response: Some(CommonResponse {
                    status: 0,
                    header_mutation: Some(mutation),
                    body_mutation: None,
                }),
            },
        )),
    })
}

/// A Conduit with one `ext_proc` entry in front of the echo upstream: the server and the upstream's port.
fn start(entry_config: serde_json::Value) -> common::TestServer {
    let (upstream, _upstream_thread) = common::start_echo_upstream();
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": format!("http://127.0.0.1:{upstream}"),
                "middleware": [{ "type": "ext_proc", "config": entry_config }]
            }]
        }),
    )
}

fn echoed(resp: reqwest::blocking::Response) -> serde_json::Value {
    resp.json().expect("the echo upstream answers with JSON")
}

#[cfg(feature = "proxy")]
#[test]
fn a_header_mutation_of_the_processor_reaches_the_upstream() {
    let (processor, seen) = support::serve_in_background(mutation(HeaderMutation {
        set_headers: vec![
            option(
                "x-from-processor",
                "yes",
                append_action::OVERWRITE_IF_EXISTS_OR_ADD,
            ),
            option(
                "x-user",
                "overwritten",
                append_action::OVERWRITE_IF_EXISTS_OR_ADD,
            ),
        ],
        remove_headers: vec!["x-remove-me".into()],
    }));
    let server = start(serde_json::json!({ "grpc": processor }));

    let resp = common::local_client()
        .get(server.url("/orders?id=7"))
        .header("x-user", "ann")
        .header("x-remove-me", "1")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = echoed(resp);
    assert_eq!(body["headers"]["x-from-processor"], "yes");
    assert_eq!(body["headers"]["x-user"], "overwritten");
    assert!(body["headers"].get("x-remove-me").is_none(), "{body}");

    // what the processor was told: the pseudo-headers and the client's own headers
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let Some(conduit_ext_proc::proto::processing_request::Request::RequestHeaders(h)) =
        &seen[0].request
    else {
        panic!("not request headers");
    };
    let sent: std::collections::HashMap<String, String> = h
        .headers
        .as_ref()
        .unwrap()
        .headers
        .iter()
        .map(|v| {
            (
                v.key.clone(),
                String::from_utf8_lossy(&v.raw_value).into_owned(),
            )
        })
        .collect();
    assert_eq!(sent[":method"], "GET");
    assert_eq!(sent[":path"], "/orders?id=7");
    assert_eq!(sent[":scheme"], "http");
    assert_eq!(sent["x-user"], "ann");
    assert_eq!(sent["x-remove-me"], "1");
}

/// The headers that frame the request cannot be changed by a processor (request smuggling): the change is dropped and
/// the request goes on, with the rest of the answer applied.
#[cfg(feature = "proxy")]
#[test]
fn a_processor_cannot_change_the_headers_that_frame_the_request() {
    let (processor, _) = support::serve_in_background(mutation(HeaderMutation {
        set_headers: vec![
            option(
                "transfer-encoding",
                "chunked",
                append_action::OVERWRITE_IF_EXISTS_OR_ADD,
            ),
            option("x-ok", "1", append_action::OVERWRITE_IF_EXISTS_OR_ADD),
        ],
        remove_headers: vec!["host".into()],
    }));
    let server = start(serde_json::json!({ "grpc": processor }));

    let resp = common::get(server.url("/")).unwrap();
    assert_eq!(resp.status(), 200);
    let body = echoed(resp);
    assert_eq!(body["headers"]["x-ok"], "1");
    assert!(body["headers"].get("transfer-encoding").is_none(), "{body}");
    assert!(
        body["headers"].get("host").is_some(),
        "host must stay: {body}"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn an_immediate_response_of_the_processor_ends_the_request() {
    let (processor, _) = support::serve_in_background(Behaviour::Answer(ProcessingResponse {
        response: Some(processing_response::Response::ImmediateResponse(
            ImmediateResponse {
                status: Some(HttpStatus { code: 403 }),
                headers: Some(HeaderMutation {
                    set_headers: vec![option(
                        "x-reason",
                        "policy",
                        append_action::OVERWRITE_IF_EXISTS_OR_ADD,
                    )],
                    remove_headers: Vec::new(),
                }),
                body: b"denied by policy\n".to_vec(),
                grpc_status: None,
                details: String::new(),
            },
        )),
    }));
    let server = start(serde_json::json!({ "grpc": processor }));

    let resp = common::get(server.url("/secret")).unwrap();
    assert_eq!(resp.status(), 403);
    assert_eq!(resp.headers().get("x-reason").unwrap(), "policy");
    assert_eq!(resp.text().unwrap(), "denied by policy\n");
}

/// 204, 205 and 304 have no body and no `Content-Length` (RFC 9110 sections 8.6, 15.3.5, 15.3.6, 15.4.5), whatever the
/// processor put in the answer.
#[cfg(feature = "proxy")]
#[test]
fn a_bodyless_status_is_sent_without_a_body_or_a_content_length() {
    for status in [204, 304] {
        let (processor, _) = support::serve_in_background(Behaviour::Answer(ProcessingResponse {
            response: Some(processing_response::Response::ImmediateResponse(
                ImmediateResponse {
                    status: Some(HttpStatus { code: status }),
                    headers: Some(HeaderMutation {
                        set_headers: vec![option(
                            "x-tag",
                            "kept",
                            append_action::OVERWRITE_IF_EXISTS_OR_ADD,
                        )],
                        remove_headers: Vec::new(),
                    }),
                    body: b"must not be sent".to_vec(),
                    grpc_status: None,
                    details: String::new(),
                },
            )),
        }));
        let server = start(serde_json::json!({ "grpc": processor }));
        let resp = common::get(server.url("/")).unwrap();
        assert_eq!(i32::from(resp.status().as_u16()), status);
        assert_eq!(resp.headers().get("x-tag").unwrap(), "kept");
        assert!(
            resp.headers().get("content-length").is_none(),
            "{status}: {:?}",
            resp.headers()
        );
        assert_eq!(resp.text().unwrap(), "", "{status}");
    }
}

/// Fail-closed is the default: a processor that is not there refuses the request, with `statusOnError` when set.
#[cfg(feature = "proxy")]
#[test]
fn an_unreachable_processor_refuses_the_request_unless_fail_open() {
    // a port nothing listens on
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("grpc://{}", l.local_addr().unwrap())
    };
    let closed = start(serde_json::json!({ "grpc": dead }));
    assert_eq!(common::get(closed.url("/")).unwrap().status(), 503);

    let custom = start(serde_json::json!({ "grpc": dead, "statusOnError": 502 }));
    assert_eq!(common::get(custom.url("/")).unwrap().status(), 502);

    let open = start(serde_json::json!({ "grpc": dead, "failOpen": true }));
    let resp = common::get(open.url("/")).unwrap();
    assert_eq!(
        resp.status(),
        200,
        "failOpen lets the request through to the upstream"
    );
    assert_eq!(echoed(resp)["method"], "GET");
}

#[cfg(feature = "proxy")]
#[test]
fn a_processor_that_is_late_is_cut_off_at_timeout_ms() {
    let (processor, _) = support::serve_in_background(Behaviour::Hang);
    let server = start(serde_json::json!({ "grpc": processor, "timeoutMs": 150 }));
    let started = std::time::Instant::now();
    let resp = common::get(server.url("/")).unwrap();
    assert_eq!(resp.status(), 503);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// `conduit validate` refuses a config that cannot work, with the field in the message.
#[test]
fn validate_names_the_field_of_a_bad_ext_proc_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conduit.json");
    std::fs::write(
        &path,
        serde_json::json!({ "port": 8080, "middleware": [{ "type": "ext_proc", "config": { "grpc": "grpc://nohost", "timeoutMs": 0 } }] })
            .to_string(),
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_conduit"))
        .args(["validate", "-c"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("middleware[0].config.grpc"), "{text}");
    assert!(text.contains("middleware[0].config.timeoutMs"), "{text}");
}

// ── Bodies and the response phase (issue #27, the rest of the protocol) ────────────────────────────────────────────

/// The request line and headers of one request: `(method, path, headers by lower-case name)`.
fn read_request_head(
    reader: &mut impl std::io::BufRead,
) -> Option<(String, String, serde_json::Map<String, serde_json::Value>)> {
    let mut first = String::new();
    if reader.read_line(&mut first).unwrap_or(0) == 0 {
        return None;
    }
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut headers = serde_json::Map::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.trim_end().split_once(": ") {
            headers.insert(
                k.to_ascii_lowercase(),
                serde_json::Value::String(v.to_owned()),
            );
        }
    }
    Some((method, path, headers))
}

/// The body of a request: by `Content-Length`, or chunked.
fn read_request_body(
    reader: &mut impl std::io::BufRead,
    headers: &serde_json::Map<String, serde_json::Value>,
) -> Vec<u8> {
    let header = |name: &str| headers.get(name).and_then(|v| v.as_str());
    let mut body = Vec::new();
    if let Some(len) = header("content-length") {
        body.resize(len.parse().unwrap_or(0), 0);
        let _ = reader.read_exact(&mut body);
    } else if header("transfer-encoding") == Some("chunked") {
        loop {
            let mut size = String::new();
            if reader.read_line(&mut size).unwrap_or(0) == 0 {
                break;
            }
            let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            let mut chunk = vec![0u8; n];
            let _ = reader.read_exact(&mut chunk);
            let mut crlf = String::new();
            let _ = reader.read_line(&mut crlf);
            if n == 0 {
                break;
            }
            body.extend(chunk);
        }
    }
    body
}

/// The upstream's answer to a path: `/static` and `/fail` are fixed, anything else echoes the request as JSON.
fn upstream_answer(
    method: &str,
    path: &str,
    headers: &serde_json::Map<String, serde_json::Value>,
    body: &[u8],
) -> (&'static str, &'static str, String) {
    match path {
        "/static" => (
            "200 OK",
            "X-Secret: s3\r\nX-Keep: 1\r\n",
            "upstream says hi".to_owned(),
        ),
        "/fail" => ("500 Internal Server Error", "", "boom".to_owned()),
        _ => (
            "200 OK",
            "",
            serde_json::json!({
                "method": method, "path": path, "headers": headers,
                "body": String::from_utf8_lossy(body),
            })
            .to_string(),
        ),
    }
}

/// One connection of the upstream: read a request, answer it, close.
fn serve_upstream_connection(mut stream: std::net::TcpStream) {
    use std::io::{BufReader, Write};
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let Some((method, path, headers)) = read_request_head(&mut reader) else {
        return;
    };
    let body = read_request_body(&mut reader, &headers);
    let (status, extra, payload) = upstream_answer(&method, &path, &headers, &body);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// What a body-aware upstream saw, as JSON: `{ method, path, headers, body }`; `/static` answers with a fixed body and a
/// secret header instead, `/fail` with a 500.
#[cfg(feature = "proxy")]
fn start_body_upstream() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || serve_upstream_connection(stream));
        }
    });
    port
}

#[cfg(feature = "proxy")]
fn start_body_site(entry_config: serde_json::Value) -> common::TestServer {
    let upstream = start_body_upstream();
    let port = common::free_port();
    let admin_port = common::free_port();
    common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": format!("http://127.0.0.1:{upstream}"),
                "middleware": [{ "type": "ext_proc", "config": entry_config }]
            }]
        }),
    )
}

fn phase(request: bool, body: bool, common_response: CommonResponse) -> ProcessingResponse {
    let phase = PhaseResponse {
        response: Some(common_response),
    };
    ProcessingResponse {
        response: Some(match (request, body) {
            (true, false) => processing_response::Response::RequestHeaders(phase),
            (true, true) => processing_response::Response::RequestBody(phase),
            (false, false) => processing_response::Response::ResponseHeaders(phase),
            (false, true) => processing_response::Response::ResponseBody(phase),
        }),
    }
}

fn replace_body(request: bool, text: &str) -> ProcessingResponse {
    phase(
        request,
        true,
        CommonResponse {
            status: 0,
            header_mutation: None,
            body_mutation: Some(BodyMutation {
                mutation: Some(body_mutation::Mutation::Body(text.as_bytes().to_vec())),
            }),
        },
    )
}

fn deny(status: i32, body: &str) -> ProcessingResponse {
    ProcessingResponse {
        response: Some(processing_response::Response::ImmediateResponse(
            ImmediateResponse {
                status: Some(HttpStatus { code: status }),
                headers: Some(HeaderMutation {
                    set_headers: vec![option(
                        "x-by",
                        "processor",
                        append_action::OVERWRITE_IF_EXISTS_OR_ADD,
                    )],
                    remove_headers: Vec::new(),
                }),
                body: body.as_bytes().to_vec(),
                grpc_status: None,
                details: String::new(),
            },
        )),
    }
}

fn kinds(seen: &std::sync::Mutex<Vec<ProcessingRequest>>) -> Vec<&'static str> {
    use processing_request::Request as R;
    seen.lock()
        .unwrap()
        .iter()
        .map(|m| match &m.request {
            Some(R::RequestHeaders(_)) => "request-headers",
            Some(R::RequestBody(_)) => "request-body",
            Some(R::ResponseHeaders(_)) => "response-headers",
            Some(R::ResponseBody(_)) => "response-body",
            _ => "other",
        })
        .collect()
}

fn body_text(m: &ProcessingRequest) -> Option<String> {
    use processing_request::Request as R;
    match &m.request {
        Some(R::RequestBody(b) | R::ResponseBody(b)) => {
            Some(String::from_utf8_lossy(&b.body).into_owned())
        }
        _ => None,
    }
}

/// A processor for the body tests: the headers pass, a body message is answered by `on_body`.
fn body_processor(
    on_body: impl Fn(&str) -> ProcessingResponse + Send + Sync + 'static,
) -> Behaviour {
    use processing_request::Request as R;
    script(move |m| match &m.request {
        Some(R::RequestHeaders(_)) => Reply::Answer(phase(true, false, CommonResponse::default())),
        Some(R::ResponseHeaders(_)) => {
            Reply::Answer(phase(false, false, CommonResponse::default()))
        }
        Some(R::RequestBody(b)) => Reply::Answer(on_body(&String::from_utf8_lossy(&b.body))),
        _ => Reply::Nothing,
    })
}

#[cfg(feature = "proxy")]
#[test]
fn a_buffered_request_body_is_rewritten_by_the_processor_and_framed_as_chunked() {
    let (processor, seen) = support::serve_in_background(body_processor(|body| {
        replace_body(true, &body.to_uppercase())
    }));
    let server =
        start_body_site(serde_json::json!({ "grpc": processor, "requestBody": "buffered" }));

    let resp = common::local_client()
        .post(server.url("/echo"))
        .body("hello world")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let upstream = echoed(resp);
    assert_eq!(upstream["body"], "HELLO WORLD", "{upstream}");
    assert!(
        upstream["headers"].get("content-length").is_none(),
        "{upstream}"
    );
    assert_eq!(upstream["headers"]["transfer-encoding"], "chunked");
    assert_eq!(kinds(&seen), ["request-headers", "request-body"]);
    assert_eq!(
        body_text(&seen.lock().unwrap()[1]).as_deref(),
        Some("hello world")
    );
}

#[cfg(feature = "proxy")]
#[test]
fn a_streamed_request_body_is_answered_chunk_by_chunk() {
    let (processor, _) = support::serve_in_background(body_processor(|body| {
        replace_body(true, &body.replace("secret", "******"))
    }));
    let server =
        start_body_site(serde_json::json!({ "grpc": processor, "requestBody": "streamed" }));
    let resp = common::local_client()
        .post(server.url("/echo"))
        .body("a secret and another secret")
        .send()
        .unwrap();
    assert_eq!(echoed(resp)["body"], "a ****** and another ******");
}

/// The body of a chunked POST sent as two chunks back to back on a raw socket, then the answer's status line and body.
#[cfg(feature = "proxy")]
fn post_two_chunks(port: u16, first: &str, second: &str) -> (String, String) {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .unwrap();
    let chunk = |data: &str| format!("{:x}\r\n{data}\r\n", data.len());
    let request = format!(
        "POST /echo HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{}{}0\r\n\r\n",
        chunk(first),
        chunk(second)
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    let status = answer.lines().next().unwrap_or("").to_owned();
    let body = answer.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
    (status, body)
}

#[cfg(feature = "proxy")]
#[test]
fn the_time_a_processor_takes_is_not_charged_to_the_clients_upload_rate() {
    // minUploadRateBytesPerSec = 1000: a client that sent 1000 bytes has one second of credit, and the next chunk may come
    // up to a second later. The processor holds the first chunk for 3 s while the second is already waiting in the socket;
    // measured from the arrival of the first chunk that would be 3 s late and a 408, measured from the end of the
    // exchange it is on time.
    let (processor, _) = support::serve_in_background(body_processor(|body| {
        if body.starts_with('A') {
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        replace_body(true, body)
    }));
    let upstream = start_body_upstream();
    let port = common::free_port();
    let admin_port = common::free_port();
    let _server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": format!("http://127.0.0.1:{upstream}"),
                "limits": { "minUploadRateBytesPerSec": 1000 },
                "middleware": [{
                    "type": "ext_proc",
                    "config": { "grpc": processor, "requestBody": "streamed", "timeoutMs": 10000 }
                }]
            }]
        }),
    );
    let first = format!("A{}", "x".repeat(999));
    let (status, body) = post_two_chunks(port, &first, "0123456789");
    assert!(status.contains("200"), "{status} {body}");
    assert!(body.contains("0123456789"), "{body}");
}

#[cfg(feature = "proxy")]
#[test]
fn a_processor_that_denies_a_body_ends_the_request_with_its_own_answer() {
    let (processor, _) = support::serve_in_background(body_processor(|body| {
        if body.contains("attack") {
            deny(403, "blocked by policy\n")
        } else {
            replace_body(true, body)
        }
    }));
    let server =
        start_body_site(serde_json::json!({ "grpc": processor, "requestBody": "buffered" }));
    let client = common::local_client();

    let resp = client
        .post(server.url("/echo"))
        .body("an attack")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 403);
    assert_eq!(resp.headers().get("x-by").unwrap(), "processor");
    assert_eq!(
        resp.headers().get("connection").map(|v| v.as_bytes()),
        Some(&b"close"[..]),
        "the connection is closed after the answer and the client is told so"
    );
    assert_eq!(resp.text().unwrap(), "blocked by policy\n");

    // the next request on the same server is unaffected
    let resp = client
        .post(server.url("/echo"))
        .body("fine")
        .send()
        .unwrap();
    assert_eq!(echoed(resp)["body"], "fine");
}

#[cfg(feature = "proxy")]
#[test]
fn a_buffered_body_over_max_body_bytes_is_refused_with_413() {
    let (processor, seen) =
        support::serve_in_background(body_processor(|body| replace_body(true, body)));
    let server = start_body_site(
        serde_json::json!({ "grpc": processor, "requestBody": "buffered", "maxBodyBytes": 8 }),
    );
    let resp = common::local_client()
        .post(server.url("/echo"))
        .body("0123456789abcdef")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 413);
    assert_eq!(
        kinds(&seen),
        ["request-headers"],
        "the oversized body was never sent to the processor"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn a_processor_that_drops_the_stream_in_the_middle_of_a_body_follows_the_policy() {
    use processing_request::Request as R;
    let behaviour = || {
        script(|m| match &m.request {
            Some(R::RequestHeaders(_)) => {
                Reply::Answer(phase(true, false, CommonResponse::default()))
            }
            _ => Reply::Close,
        })
    };
    let (processor, _) = support::serve_in_background(behaviour());
    let closed =
        start_body_site(serde_json::json!({ "grpc": processor, "requestBody": "buffered" }));
    let resp = common::local_client()
        .post(closed.url("/echo"))
        .body("x")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 503, "fail-closed by default");

    let (processor, _) = support::serve_in_background(behaviour());
    let open = start_body_site(
        serde_json::json!({ "grpc": processor, "requestBody": "buffered", "failOpen": true }),
    );
    let resp = common::local_client()
        .post(open.url("/echo"))
        .body("kept as it was")
        .send()
        .unwrap();
    assert_eq!(echoed(resp)["body"], "kept as it was");
}

#[cfg(feature = "proxy")]
#[test]
fn the_request_headers_are_not_sent_when_request_phase_is_false() {
    let (processor, seen) =
        support::serve_in_background(body_processor(|body| replace_body(true, body)));
    let server = start_body_site(serde_json::json!({
        "grpc": processor, "requestPhase": false, "requestBody": "buffered"
    }));
    let resp = common::local_client()
        .post(server.url("/echo"))
        .body("only the body")
        .send()
        .unwrap();
    assert_eq!(echoed(resp)["body"], "only the body");
    assert_eq!(kinds(&seen), ["request-body"]);
}

#[cfg(feature = "proxy")]
#[test]
fn the_response_headers_are_changed_by_the_processor() {
    use processing_request::Request as R;
    let (processor, seen) = support::serve_in_background(script(|m| match &m.request {
        Some(R::RequestHeaders(_)) => Reply::Answer(phase(true, false, CommonResponse::default())),
        Some(R::ResponseHeaders(_)) => Reply::Answer(phase(
            false,
            false,
            CommonResponse {
                status: 0,
                header_mutation: Some(HeaderMutation {
                    set_headers: vec![option(
                        "x-seen",
                        "by-processor",
                        append_action::OVERWRITE_IF_EXISTS_OR_ADD,
                    )],
                    remove_headers: vec!["x-secret".into()],
                }),
                body_mutation: None,
            },
        )),
        _ => Reply::Nothing,
    }));
    let server = start_body_site(serde_json::json!({ "grpc": processor, "responsePhase": true }));

    let resp = common::get(server.url("/static")).unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers().get("x-seen").unwrap(), "by-processor");
    assert!(
        resp.headers().get("x-secret").is_none(),
        "{:?}",
        resp.headers()
    );
    assert_eq!(resp.headers().get("x-keep").unwrap(), "1");
    assert_eq!(resp.text().unwrap(), "upstream says hi");

    // the processor was told the upstream's status and headers
    assert_eq!(kinds(&seen), ["request-headers", "response-headers"]);
    let seen = seen.lock().unwrap();
    let Some(R::ResponseHeaders(h)) = &seen[1].request else {
        panic!("not response headers");
    };
    let sent: std::collections::HashMap<String, String> = h
        .headers
        .as_ref()
        .unwrap()
        .headers
        .iter()
        .map(|v| {
            (
                v.key.clone(),
                String::from_utf8_lossy(&v.raw_value).into_owned(),
            )
        })
        .collect();
    assert_eq!(sent[":status"], "200");
    assert_eq!(sent["x-secret"], "s3");
}

#[cfg(feature = "proxy")]
#[test]
fn a_processor_can_replace_the_response_it_is_shown() {
    use processing_request::Request as R;
    let (processor, _) = support::serve_in_background(script(|m| match &m.request {
        Some(R::RequestHeaders(_)) => Reply::Answer(phase(true, false, CommonResponse::default())),
        Some(R::ResponseHeaders(h)) => {
            let status = h
                .headers
                .as_ref()
                .and_then(|map| map.headers.iter().find(|v| v.key == ":status"))
                .map(|v| String::from_utf8_lossy(&v.raw_value).into_owned());
            if status.as_deref() == Some("500") {
                Reply::Answer(deny(503, "{\"error\":\"masked\"}"))
            } else {
                Reply::Answer(phase(false, false, CommonResponse::default()))
            }
        }
        _ => Reply::Nothing,
    }));
    let server = start_body_site(serde_json::json!({ "grpc": processor, "responsePhase": true }));

    let resp = common::get(server.url("/fail")).unwrap();
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers().get("x-by").unwrap(), "processor");
    assert!(resp.headers().get("x-secret").is_none());
    assert_eq!(
        resp.text().unwrap(),
        "{\"error\":\"masked\"}",
        "the upstream's body is gone"
    );

    let resp = common::get(server.url("/static")).unwrap();
    assert_eq!(
        resp.text().unwrap(),
        "upstream says hi",
        "other responses are untouched"
    );
}

/// `GET /static` through an entry that observes the response body (plus whatever `extra` adds to its config): the client's
/// answer, and what the processor was sent as the response body.
#[cfg(feature = "proxy")]
fn observed_get(extra: serde_json::Value) -> (String, String) {
    use processing_request::Request as R;
    let (processor, seen) = support::serve_in_background(script(|m| match &m.request {
        Some(R::RequestHeaders(_)) => Reply::Answer(phase(true, false, CommonResponse::default())),
        Some(R::RequestBody(_)) => Reply::Answer(phase(true, true, CommonResponse::default())),
        _ => Reply::Nothing,
    }));
    let mut entry = serde_json::json!({ "grpc": processor, "responseBody": "observe" });
    entry
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let server = start_body_site(entry);

    let resp = common::get(server.url("/static")).unwrap();
    assert_eq!(resp.status(), 200);
    let answer = resp.text().unwrap();

    let mut copied = String::new();
    for _ in 0..100 {
        copied = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|m| matches!(m.request, Some(R::ResponseBody(_))))
            .filter_map(body_text)
            .collect();
        if copied == "upstream says hi" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    (answer, copied)
}

#[cfg(feature = "proxy")]
#[test]
fn an_observed_response_body_is_copied_to_the_processor_and_the_client_gets_it_unchanged() {
    let (answer, copied) = observed_get(serde_json::json!({}));
    assert_eq!(answer, "upstream says hi");
    assert_eq!(copied, "upstream says hi");
}

#[cfg(feature = "proxy")]
#[test]
fn a_response_is_observed_for_a_bodyless_get_when_the_request_body_is_exchanged_too() {
    // The request body phase is not run for a GET with no body: the request body is done all the same, or the stream would
    // be kept for it for ever and the response never observed (found by scripts/e2e/ext-proc.sh).
    let (answer, copied) = observed_get(serde_json::json!({ "requestBody": "buffered" }));
    assert_eq!(answer, "upstream says hi");
    assert_eq!(copied, "upstream says hi");
}

#[cfg(feature = "proxy")]
#[test]
fn a_response_phase_failure_is_refused_or_ignored_by_the_policy() {
    let (processor, _) = support::serve_in_background(script(|m| match &m.request {
        Some(processing_request::Request::RequestHeaders(_)) => {
            Reply::Answer(phase(true, false, CommonResponse::default()))
        }
        _ => Reply::Close,
    }));
    let closed = start_body_site(serde_json::json!({ "grpc": processor, "responsePhase": true }));
    let resp = common::get(closed.url("/static")).unwrap();
    assert_eq!(
        resp.status(),
        503,
        "the processor ended the stream instead of answering: fail-closed"
    );

    let (processor, _) = support::serve_in_background(script(|m| match &m.request {
        Some(processing_request::Request::RequestHeaders(_)) => {
            Reply::Answer(phase(true, false, CommonResponse::default()))
        }
        _ => Reply::Close,
    }));
    let open = start_body_site(
        serde_json::json!({ "grpc": processor, "responsePhase": true, "failOpen": true }),
    );
    let resp = common::get(open.url("/static")).unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "upstream says hi");
}

#[cfg(feature = "proxy")]
#[test]
fn an_upgrade_header_does_not_switch_the_body_phases_off() {
    // The upstream may ignore `Upgrade` (RFC 9110 section 7.8), so the request is an ordinary one with a body, and a
    // fail-closed body processor must still see it (the security review of #749).
    let (processor, seen) = support::serve_in_background(body_processor(|body| {
        if body.contains("attack") {
            deny(403, "blocked by policy\n")
        } else {
            replace_body(true, body)
        }
    }));
    let server =
        start_body_site(serde_json::json!({ "grpc": processor, "requestBody": "buffered" }));
    let client = common::local_client();
    let resp = client
        .post(server.url("/echo"))
        .header("upgrade", "websocket")
        .body("an attack")
        .send()
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "the body was not shown to the processor"
    );
    assert!(kinds(&seen).contains(&"request-body"));
}

#[cfg(feature = "proxy")]
#[test]
fn a_get_without_a_body_is_not_shown_to_the_body_phase() {
    // The upstream request of a bodyless GET is not framed for a body, so whatever a processor made up for it would be
    // dropped by the writer: the body phase is not run, and the request goes through as it came.
    let (processor, seen) = support::serve_in_background(body_processor(|_| {
        replace_body(true, "made up by the processor")
    }));
    let server =
        start_body_site(serde_json::json!({ "grpc": processor, "requestBody": "buffered" }));
    let resp = common::local_client()
        .get(server.url("/echo"))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(echoed(resp)["body"], "");
    assert_eq!(kinds(&seen), ["request-headers"]);
}

#[cfg(feature = "proxy")]
#[test]
fn a_refusal_after_100_continue_still_delivers_the_final_answer() {
    use std::io::{Read, Write};
    // `Expect: 100-continue`: Pingora answers `100 Continue` when the body is first read, and that interim response is not
    // an answer yet: the processor's refusal of the body must still arrive as the final response.
    let (processor, _) = support::serve_in_background(body_processor(|body| {
        if body.contains("attack") {
            deny(403, "blocked by policy\n")
        } else {
            replace_body(true, body)
        }
    }));
    let server =
        start_body_site(serde_json::json!({ "grpc": processor, "requestBody": "buffered" }));
    let mut client = std::net::TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    client
        .write_all(
            b"POST /echo HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: 9\r\nConnection: close\r\n\r\nan attack",
        )
        .unwrap();
    let mut answer = String::new();
    let _ = client.read_to_string(&mut answer);
    assert!(answer.contains("403"), "no final answer: {answer:?}");
    assert!(answer.contains("blocked by policy"), "{answer:?}");
}

#[cfg(feature = "proxy")]
#[test]
fn the_tunnel_after_a_101_is_not_sent_to_a_response_body_observer() {
    use std::io::{Read, Write};
    // An upstream that accepts the upgrade, says something through the tunnel and reads what comes back.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\nfrom-upstream",
                );
                let mut sink = [0u8; 64];
                while stream.read(&mut sink).unwrap_or(0) > 0 {}
            });
        }
    });
    let (processor, seen) = support::serve_in_background(body_processor(|_| {
        phase(true, true, CommonResponse::default())
    }));
    let port = common::free_port();
    let admin_port = common::free_port();
    let _server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": { "/": { "targets": [format!("http://127.0.0.1:{upstream}")], "websocket": true } },
                "middleware": [{
                    "type": "ext_proc",
                    "config": { "grpc": processor, "responsePhase": true, "responseBody": "observe" }
                }]
            }]
        }),
    );

    let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    client
        .write_all(
            b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        )
        .unwrap();
    let mut received = String::new();
    let mut buf = [0u8; 256];
    while !received.contains("from-upstream") {
        let n = client.read(&mut buf).unwrap();
        assert!(n > 0, "the tunnel closed early: {received:?}");
        received.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    assert!(received.starts_with("HTTP/1.1 101"), "{received}");
    client.write_all(b"from-client").unwrap();
    // the observer is fed in the background: give it the time it would need
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(
        kinds(&seen),
        ["request-headers", "response-headers"],
        "tunnel bytes must not reach the processor as a response body"
    );
}

#[cfg(feature = "proxy")]
#[test]
fn a_refusal_of_the_response_phase_is_not_undone_by_the_routes_retry_on_5xx() {
    // A fail-closed response phase refuses with 503; a route that retries on 5xx must not send the request upstream again
    // and let the next response go to the client unseen (the security review of #749).
    let upstream = start_body_upstream();
    let (processor, _) = support::serve_in_background(script(|m| match &m.request {
        Some(processing_request::Request::RequestHeaders(_)) => {
            Reply::Answer(phase(true, false, CommonResponse::default()))
        }
        _ => Reply::Close,
    }));
    let port = common::free_port();
    let admin_port = common::free_port();
    let server = common::TestServer::start_with_config(
        port,
        admin_port,
        serde_json::json!({
            "global": { "admin": { "bind": format!("127.0.0.1:{admin_port}") } },
            "sites": [{
                "port": port,
                "proxy": { "/": {
                    "targets": [format!("http://127.0.0.1:{upstream}")],
                    "retry": { "attempts": 3, "conditions": ["5xx"] }
                } },
                "middleware": [{ "type": "ext_proc",
                                 "config": { "grpc": processor, "responsePhase": true } }]
            }]
        }),
    );
    let resp = common::get(server.url("/static")).unwrap();
    assert_eq!(resp.status(), 503, "the refusal stands: {:?}", resp.text());
}
