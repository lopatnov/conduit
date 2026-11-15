// Tests that proxy real traffic are compiled out without the `proxy` feature, which leaves their helpers unused there.
#![cfg_attr(not(feature = "proxy"), allow(dead_code))]
//! A whole Conduit in front of an in-process `ext_proc` gRPC processor (issue #27, slice 1): the request headers go to
//! the processor, its header mutations reach the upstream, its immediate response reaches the client, and a processor
//! that fails is handled by the entry's policy (fail-closed by default).

mod common;
#[path = "../crates/conduit-ext-proc/tests/support/mod.rs"]
mod support;

use conduit_ext_proc::proto::{
    append_action, processing_response, CommonResponse, HeaderMutation, HeaderValue,
    HeaderValueOption, HttpStatus, ImmediateResponse, PhaseResponse, ProcessingResponse,
};
use support::Behaviour;

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
