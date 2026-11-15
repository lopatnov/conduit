//! The client against an in-process processor: a real gRPC server on a loopback port that answers with whatever the test
//! says (port 0, no `protoc`; the messages are the crate's own, whose wire bytes `proto::tests` pin).
//!
//! A unit-test module of the library (not `tests/`): the SonarCloud coverage run measures `cargo llvm-cov --lib` only, so
//! a client exercised from `tests/` would count as uncovered.

use std::time::Duration;

use crate::client::{channel_for, Failure};
use crate::proto::{
    append_action, processing_request, processing_response, CommonResponse, HeaderMutation,
    HeaderValue as PbValue, HeaderValueOption, HttpStatus, ImmediateResponse, PhaseResponse,
    ProcessingResponse,
};
use crate::support::{dead_address, serve, Behaviour};
use crate::{clear_channels, DirectResponse, ExtProcClient, ExtProcConfig, RequestInfo, Verdict};
use http::{HeaderMap, HeaderValue};

fn config(grpc: &str, extra: serde_json::Value) -> ExtProcConfig {
    let mut value = serde_json::json!({ "grpc": grpc });
    if let (Some(base), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        base.extend(extra.clone());
    }
    ExtProcConfig::from_value(Some(&value)).expect("a valid config")
}

fn headers_answer(mutation: HeaderMutation) -> ProcessingResponse {
    ProcessingResponse {
        response: Some(processing_response::Response::RequestHeaders(
            PhaseResponse {
                response: Some(CommonResponse {
                    status: 0,
                    header_mutation: Some(mutation),
                    body_mutation: None,
                }),
            },
        )),
    }
}

fn option(name: &str, value: &str, action: i32) -> HeaderValueOption {
    HeaderValueOption {
        header: Some(PbValue {
            key: name.into(),
            value: String::new(),
            raw_value: value.as_bytes().to_vec(),
        }),
        append: None,
        append_action: action,
        keep_empty_value: false,
    }
}

async fn ask(client: &ExtProcClient) -> Verdict {
    let mut headers = HeaderMap::new();
    headers.insert("x-user", HeaderValue::from_static("ann"));
    client
        .process_request_headers(&RequestInfo {
            method: "GET",
            scheme: "http",
            authority: "example.com",
            path: "/a?b=1",
            headers: &headers,
            end_of_stream: true,
        })
        .await
}

fn refused(verdict: &Verdict, status: u16) {
    match verdict {
        Verdict::Respond(DirectResponse { status: s, .. }) => assert_eq!(*s, status, "{verdict:?}"),
        other => panic!("expected a refusal with {status}, got {other:?}"),
    }
}

fn unchanged(verdict: &Verdict) {
    match verdict {
        Verdict::Continue(changes) => assert!(changes.is_empty(), "{changes:?}"),
        other => panic!("expected the request to continue unchanged, got {other:?}"),
    }
}

#[tokio::test]
async fn the_processor_sees_the_request_and_its_header_changes_are_returned() {
    let (address, seen) = serve(Behaviour::Answer(headers_answer(HeaderMutation {
        set_headers: vec![
            option(
                "X-Tenant",
                "acme",
                append_action::OVERWRITE_IF_EXISTS_OR_ADD,
            ),
            option("x-extra", "1", append_action::APPEND_IF_EXISTS_OR_ADD),
        ],
        remove_headers: vec!["x-user".into()],
    })))
    .await;
    let client = ExtProcClient::new(&config(&address, serde_json::json!({})));

    let Verdict::Continue(changes) = ask(&client).await else {
        panic!("expected the request to continue");
    };
    let set: Vec<(String, String)> = changes
        .set
        .iter()
        .map(|s| {
            (
                s.name.as_str().to_owned(),
                s.value.to_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        set,
        [
            ("x-tenant".to_owned(), "acme".to_owned()),
            ("x-extra".to_owned(), "1".to_owned())
        ]
    );
    assert_eq!(
        changes
            .remove
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>(),
        ["x-user"]
    );

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let Some(processing_request::Request::RequestHeaders(h)) = &seen[0].request else {
        panic!("not request headers");
    };
    assert!(h.end_of_stream);
    let keys: Vec<(String, Vec<u8>)> = h
        .headers
        .as_ref()
        .unwrap()
        .headers
        .iter()
        .map(|v| (v.key.clone(), v.raw_value.clone()))
        .collect();
    assert_eq!(
        keys,
        [
            (":method".to_owned(), b"GET".to_vec()),
            (":scheme".to_owned(), b"http".to_vec()),
            (":authority".to_owned(), b"example.com".to_vec()),
            (":path".to_owned(), b"/a?b=1".to_vec()),
            ("x-user".to_owned(), b"ann".to_vec()),
        ]
    );
}

#[tokio::test]
async fn an_empty_answer_continues_the_request_unchanged() {
    let (address, _) = serve(Behaviour::Answer(ProcessingResponse {
        response: Some(processing_response::Response::RequestHeaders(
            PhaseResponse::default(),
        )),
    }))
    .await;
    let client = ExtProcClient::new(&config(&address, serde_json::json!({})));
    unchanged(&ask(&client).await);
}

#[tokio::test]
async fn an_immediate_response_ends_the_request_with_its_status_headers_and_body() {
    let (address, _) = serve(Behaviour::Answer(ProcessingResponse {
        response: Some(processing_response::Response::ImmediateResponse(
            ImmediateResponse {
                status: Some(HttpStatus { code: 403 }),
                headers: Some(HeaderMutation {
                    set_headers: vec![option(
                        "x-reason",
                        "blocked",
                        append_action::OVERWRITE_IF_EXISTS_OR_ADD,
                    )],
                    remove_headers: Vec::new(),
                }),
                body: b"no entry\n".to_vec(),
                grpc_status: None,
                details: "policy".into(),
            },
        )),
    }))
    .await;
    let client = ExtProcClient::new(&config(&address, serde_json::json!({})));

    let Verdict::Respond(answer) = ask(&client).await else {
        panic!("expected an immediate response");
    };
    assert_eq!(answer.status, 403);
    assert_eq!(&answer.body[..], b"no entry\n");
    assert_eq!(answer.headers.len(), 1);
    assert_eq!(answer.headers[0].0.as_str(), "x-reason");
    assert_eq!(answer.headers[0].1, "blocked");
}

#[tokio::test]
async fn an_immediate_response_with_a_status_outside_200_to_599_is_a_failure() {
    let bad = |code| {
        Behaviour::Answer(ProcessingResponse {
            response: Some(processing_response::Response::ImmediateResponse(
                ImmediateResponse {
                    status: Some(HttpStatus { code }),
                    ..ImmediateResponse::default()
                },
            )),
        })
    };
    for code in [0, 99, 600] {
        let (address, _) = serve(bad(code)).await;
        let closed = ExtProcClient::new(&config(
            &address,
            serde_json::json!({ "statusOnError": 502 }),
        ));
        refused(&ask(&closed).await, 502);
        let open = ExtProcClient::new(&config(&address, serde_json::json!({ "failOpen": true })));
        unchanged(&ask(&open).await);
    }
}

#[tokio::test]
async fn a_processor_that_never_answers_is_cut_off_at_the_timeout() {
    let (address, _) = serve(Behaviour::Hang).await;
    let started = std::time::Instant::now();
    let closed = ExtProcClient::new(&config(&address, serde_json::json!({ "timeoutMs": 100 })));
    refused(&ask(&closed).await, 503);
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "{:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );

    let open = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "timeoutMs": 100, "failOpen": true }),
    ));
    unchanged(&ask(&open).await);
}

#[tokio::test]
async fn an_unreachable_processor_refuses_with_status_on_error_or_continues_when_failing_open() {
    let address = dead_address().await;
    let closed = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "statusOnError": 504 }),
    ));
    refused(&ask(&closed).await, 504);

    let open = ExtProcClient::new(&config(&address, serde_json::json!({ "failOpen": true })));
    unchanged(&ask(&open).await);
}

#[tokio::test]
async fn a_stream_closed_without_an_answer_and_a_grpc_error_follow_the_policy() {
    for behaviour in [
        Behaviour::Close,
        Behaviour::Fail(tonic::Code::Unavailable, "overloaded"),
        Behaviour::Fail(tonic::Code::Internal, "boom"),
    ] {
        let (address, _) = serve(behaviour).await;
        let closed = ExtProcClient::new(&config(&address, serde_json::json!({})));
        refused(&ask(&closed).await, 503);
        let open = ExtProcClient::new(&config(&address, serde_json::json!({ "failOpen": true })));
        unchanged(&ask(&open).await);
    }
}

#[tokio::test]
async fn an_answer_the_proxy_did_not_ask_for_or_may_not_accept_follows_the_policy() {
    let wrong_phase = ProcessingResponse {
        response: Some(processing_response::Response::ResponseHeaders(
            PhaseResponse::default(),
        )),
    };
    let empty = ProcessingResponse { response: None };
    // one entry that is not a header at all refuses the whole answer, the valid one beside it is not applied
    let malformed = headers_answer(HeaderMutation {
        set_headers: vec![
            option("x-fine", "1", append_action::OVERWRITE_IF_EXISTS_OR_ADD),
            option("bad name", "x", 0),
        ],
        remove_headers: Vec::new(),
    });
    for answer in [wrong_phase, empty, malformed] {
        let (address, _) = serve(Behaviour::Answer(answer)).await;
        let closed = ExtProcClient::new(&config(&address, serde_json::json!({})));
        refused(&ask(&closed).await, 503);
        let open = ExtProcClient::new(&config(&address, serde_json::json!({ "failOpen": true })));
        unchanged(&ask(&open).await);
    }
}

/// `content-length` frames the request: a processor may not change it. That is not a failure of the processor: the change
/// is dropped and the request continues, with nothing else changed.
#[tokio::test]
async fn a_change_to_a_framing_header_is_dropped_and_is_not_a_failure() {
    let (address, _) = serve(Behaviour::Answer(headers_answer(HeaderMutation {
        set_headers: vec![option(
            "content-length",
            "0",
            append_action::OVERWRITE_IF_EXISTS_OR_ADD,
        )],
        remove_headers: vec!["transfer-encoding".into()],
    })))
    .await;
    // fail-closed: if the answer were counted as a failure this would be a refusal
    let client = ExtProcClient::new(&config(&address, serde_json::json!({})));
    unchanged(&ask(&client).await);
}

#[tokio::test]
async fn one_channel_serves_many_requests_at_once() {
    let (address, seen) = serve(Behaviour::Answer(headers_answer(HeaderMutation {
        set_headers: vec![option(
            "x-n",
            "1",
            append_action::OVERWRITE_IF_EXISTS_OR_ADD,
        )],
        remove_headers: Vec::new(),
    })))
    .await;
    let client = ExtProcClient::new(&config(&address, serde_json::json!({})));
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move {
                matches!(ask(&client).await, Verdict::Continue(c) if !c.is_empty())
            })
        })
        .collect();
    for task in tasks {
        assert!(task.await.unwrap());
    }
    assert_eq!(seen.lock().unwrap().len(), 20);
}

#[tokio::test]
async fn a_processor_that_cannot_even_be_configured_fails_every_exchange_under_the_policy() {
    // an unreadable caFile: no channel can be built, and the entry's policy decides, not a panic or a skipped check
    let missing = serde_json::json!({ "caFile": "/nonexistent/conduit-ext-proc-ca.pem" });
    let closed = ExtProcClient::new(&config("grpcs://127.0.0.1:9", missing.clone()));
    refused(&ask(&closed).await, 503);
    let mut open = missing;
    open["failOpen"] = serde_json::json!(true);
    let open = ExtProcClient::new(&config("grpcs://127.0.0.1:9", open));
    unchanged(&ask(&open).await);
}

#[tokio::test]
async fn a_tls_processor_uses_the_public_roots_or_the_given_ca_file_and_fails_under_the_policy_when_down(
) {
    // grpcs:// with no caFile: the bundled public roots; nothing listens on the port, so the exchange fails
    let public = ExtProcClient::new(&config(
        "grpcs://127.0.0.1:9",
        serde_json::json!({ "timeoutMs": 500 }),
    ));
    refused(&ask(&public).await, 503);
    // with a caFile that is readable (its content is only used when a connection is made)
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(
        &ca,
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let file = serde_json::json!({ "caFile": ca.to_str().unwrap(), "timeoutMs": 500 });
    let with_file = ExtProcClient::new(&config("grpcs://127.0.0.1:9", file));
    refused(&ask(&with_file).await, 503);
}

#[tokio::test]
async fn a_reload_forgets_a_channel_that_could_not_be_built() {
    // The channel cache keeps a failure too (an unreadable caFile is read once, not per request). Mend the file: the
    // cached failure stays until a reload, and then the channel builds. The key holds a path of its own, so no other
    // test shares this cache entry.
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    let c = config(
        "grpcs://127.0.0.1:9",
        serde_json::json!({ "caFile": ca.to_str().unwrap() }),
    );
    assert!(channel_for(&c).is_err(), "no such file yet");
    std::fs::write(
        &ca,
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    assert!(
        channel_for(&c).is_err(),
        "the failure is cached until a reload"
    );
    clear_channels();
    assert!(channel_for(&c).is_ok(), "a reload reads the file again");
    clear_channels();
}

#[test]
fn every_failure_says_what_went_wrong_in_one_line() {
    let texts = [
        Failure::Unreachable("refused".into()).to_string(),
        Failure::TimedOut(Duration::from_millis(250)).to_string(),
        Failure::Closed.to_string(),
        Failure::Status("Unavailable".into()).to_string(),
        Failure::Unexpected("a trailers answer").to_string(),
        Failure::Rejected("bad\nname".into()).to_string(),
    ];
    assert!(texts[0].contains("cannot be reached: refused"), "{texts:?}");
    assert!(texts[1].contains("250 ms"), "{texts:?}");
    assert!(texts[2].contains("closed the stream"), "{texts:?}");
    assert!(texts[3].contains("error: Unavailable"), "{texts:?}");
    assert!(texts[4].contains("a trailers answer"), "{texts:?}");
    assert!(texts[5].contains("not acceptable"), "{texts:?}");
    assert!(
        texts.iter().all(|t| !t.contains('\n')),
        "a log line must stay one line: {texts:?}"
    );
}

#[tokio::test]
async fn an_answer_may_be_as_large_as_the_body_limit_allows_and_never_below_the_tonic_default() {
    let small = ExtProcClient::new(&config("grpc://127.0.0.1:1/", serde_json::json!({})));
    assert_eq!(small.decode_limit(), 4 * 1024 * 1024 + 64 * 1024);
    let big = ExtProcClient::new(&config(
        "grpc://127.0.0.1:1/",
        serde_json::json!({ "requestBody": "buffered", "maxBodyBytes": 16 * 1024 * 1024 }),
    ));
    assert_eq!(big.decode_limit(), 16 * 1024 * 1024 + 64 * 1024);
}
