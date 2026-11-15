//! The stream that stays open across the phases of one request, against a scripted in-process processor (a real gRPC server
//! on a loopback port, port 0). A unit-test module of the library so that the coverage run (`--lib`) counts it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue};

use crate::proto::{
    body_mutation, processing_request, processing_response, BodyMutation, CommonResponse,
    HeaderMutation, HeaderValue as PbValue, HeaderValueOption, HttpStatus, ImmediateResponse,
    PhaseResponse, ProcessingRequest, ProcessingResponse,
};
use crate::support::{script, serve, Behaviour, Reply};
use crate::{
    BodyVerdict, ExtProcClient, ExtProcConfig, ExtProcSessions, RequestInfo, ResponseInfo, Verdict,
};

fn config(grpc: &str, extra: serde_json::Value) -> ExtProcConfig {
    let mut value = serde_json::json!({ "grpc": grpc, "timeoutMs": 2000 });
    if let (Some(base), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        base.extend(extra.clone());
    }
    ExtProcConfig::from_value(Some(&value)).expect("a valid config")
}

fn kinds(seen: &Mutex<Vec<ProcessingRequest>>) -> Vec<&'static str> {
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

fn bodies(seen: &Mutex<Vec<ProcessingRequest>>, request: bool) -> Vec<(String, bool)> {
    use processing_request::Request as R;
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|m| match &m.request {
            Some(R::RequestBody(b)) if request => Some(b),
            Some(R::ResponseBody(b)) if !request => Some(b),
            _ => None,
        })
        .map(|b| {
            (
                String::from_utf8_lossy(&b.body).into_owned(),
                b.end_of_stream,
            )
        })
        .collect()
}

fn empty_headers_answer(request: bool) -> ProcessingResponse {
    let phase = PhaseResponse {
        response: Some(CommonResponse::default()),
    };
    ProcessingResponse {
        response: Some(if request {
            processing_response::Response::RequestHeaders(phase)
        } else {
            processing_response::Response::ResponseHeaders(phase)
        }),
    }
}

fn body_answer(request: bool, replacement: Option<&str>) -> ProcessingResponse {
    let phase = PhaseResponse {
        response: Some(CommonResponse {
            status: 0,
            header_mutation: None,
            body_mutation: replacement.map(|r| BodyMutation {
                mutation: Some(body_mutation::Mutation::Body(r.as_bytes().to_vec())),
            }),
        }),
    };
    ProcessingResponse {
        response: Some(if request {
            processing_response::Response::RequestBody(phase)
        } else {
            processing_response::Response::ResponseBody(phase)
        }),
    }
}

fn immediate(status: i32, body: &str) -> ProcessingResponse {
    ProcessingResponse {
        response: Some(processing_response::Response::ImmediateResponse(
            ImmediateResponse {
                status: Some(HttpStatus { code: status }),
                headers: Some(HeaderMutation {
                    set_headers: vec![HeaderValueOption {
                        header: Some(PbValue {
                            key: "x-by".into(),
                            value: String::new(),
                            raw_value: b"processor".to_vec(),
                        }),
                        append: None,
                        append_action: 2,
                        keep_empty_value: false,
                    }],
                    remove_headers: Vec::new(),
                }),
                body: body.as_bytes().to_vec(),
                grpc_status: None,
                details: "scripted".into(),
            },
        )),
    }
}

fn request_info(headers: &HeaderMap) -> RequestInfo<'_> {
    RequestInfo {
        method: "POST",
        scheme: "http",
        authority: "example.com",
        path: "/upload",
        headers,
        end_of_stream: false,
    }
}

/// The processor of most tests: answers headers with no change, and each body message with `on_body(text, end)`.
fn processor(on_body: impl Fn(&str, bool, bool) -> Reply + Send + Sync + 'static) -> Behaviour {
    use processing_request::Request as R;
    script(move |m| match &m.request {
        Some(R::RequestHeaders(_)) => Reply::Answer(empty_headers_answer(true)),
        Some(R::ResponseHeaders(_)) => Reply::Answer(empty_headers_answer(false)),
        Some(R::RequestBody(b)) => {
            on_body(&String::from_utf8_lossy(&b.body), b.end_of_stream, true)
        }
        Some(R::ResponseBody(b)) => {
            on_body(&String::from_utf8_lossy(&b.body), b.end_of_stream, false)
        }
        _ => Reply::Nothing,
    })
}

async fn open(client: &ExtProcClient) -> crate::ExtProcSession {
    let headers = HeaderMap::new();
    let mut session = client.session();
    assert!(matches!(
        session.request_headers(&request_info(&headers)).await,
        Verdict::Continue(_)
    ));
    session
}

#[tokio::test]
async fn a_buffered_body_is_held_until_the_end_and_sent_whole() {
    let (address, seen) = serve(processor(|body, _, _| {
        Reply::Answer(body_answer(true, Some(&body.to_uppercase())))
    }))
    .await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestBody": "buffered" }),
    ));
    let mut session = open(&client).await;
    assert!(session.handles_request_body());

    assert_eq!(
        session.request_body(Some(Bytes::from("hel")), false).await,
        BodyVerdict::Forward(None),
        "a chunk is held back"
    );
    assert_eq!(
        session.request_body(Some(Bytes::from("lo")), false).await,
        BodyVerdict::Forward(None)
    );
    assert_eq!(
        session.request_body(None, true).await,
        BodyVerdict::Forward(Some(Bytes::from("HELLO"))),
        "the processor's body replaces the whole of it"
    );
    assert_eq!(kinds(&seen), ["request-headers", "request-body"]);
    assert_eq!(bodies(&seen, true), [("hello".to_owned(), true)]);
}

#[tokio::test]
async fn a_buffered_body_the_processor_keeps_is_forwarded_as_it_was_and_an_empty_one_is_sent_too() {
    let (address, seen) = serve(processor(|_, _, _| Reply::Answer(body_answer(true, None)))).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestBody": "buffered" }),
    ));
    let mut session = open(&client).await;
    session.request_body(Some(Bytes::from("abc")), false).await;
    assert_eq!(
        session.request_body(Some(Bytes::from("d")), true).await,
        BodyVerdict::Forward(Some(Bytes::from("abcd")))
    );

    let mut empty = open(&client).await;
    assert_eq!(
        empty.request_body(None, true).await,
        BodyVerdict::Forward(None)
    );
    assert_eq!(
        bodies(&seen, true),
        [("abcd".to_owned(), true), (String::new(), true)],
        "a request with no body is still shown to the processor, as an empty one"
    );
}

#[tokio::test]
async fn a_buffered_body_over_the_cap_ends_the_request_with_413_without_asking_the_processor() {
    let (address, seen) = serve(processor(|_, _, _| Reply::Answer(body_answer(true, None)))).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestBody": "buffered", "maxBodyBytes": 4 }),
    ));
    let mut session = open(&client).await;
    session.request_body(Some(Bytes::from("abc")), false).await;
    match session.request_body(Some(Bytes::from("de")), false).await {
        BodyVerdict::Respond(answer) => assert_eq!(answer.status, 413),
        other => panic!("expected 413, got {other:?}"),
    }
    assert_eq!(kinds(&seen), ["request-headers"], "no body was sent");
    // the stream is over: what comes after is not held or sent
    assert_eq!(
        session.request_body(Some(Bytes::from("x")), true).await,
        BodyVerdict::Forward(Some(Bytes::from("x")))
    );
}

#[tokio::test]
async fn a_streamed_body_is_answered_chunk_by_chunk() {
    let (address, seen) = serve(processor(|body, end, _| {
        let replacement = match body {
            "drop" => Some(""),
            "a" => Some("A"),
            _ => None,
        };
        let _ = end;
        Reply::Answer(body_answer(true, replacement))
    }))
    .await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestBody": "streamed" }),
    ));
    let mut session = open(&client).await;

    assert_eq!(
        session.request_body(Some(Bytes::from("a")), false).await,
        BodyVerdict::Forward(Some(Bytes::from("A")))
    );
    assert_eq!(
        session.request_body(Some(Bytes::from("drop")), false).await,
        BodyVerdict::Forward(None),
        "an emptied chunk forwards nothing"
    );
    assert_eq!(
        session.request_body(Some(Bytes::from("kept")), false).await,
        BodyVerdict::Forward(Some(Bytes::from("kept")))
    );
    assert_eq!(
        session.request_body(None, true).await,
        BodyVerdict::Forward(None),
        "the end of a body with no last chunk is told to the processor too"
    );
    assert_eq!(
        bodies(&seen, true),
        [
            ("a".to_owned(), false),
            ("drop".to_owned(), false),
            ("kept".to_owned(), false),
            (String::new(), true)
        ]
    );
}

#[tokio::test]
async fn an_immediate_response_to_a_body_ends_the_request() {
    let (address, _seen) = serve(processor(|body, _, _| {
        if body.contains("secret") {
            Reply::Answer(immediate(403, "blocked"))
        } else {
            Reply::Answer(body_answer(true, None))
        }
    }))
    .await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestBody": "streamed" }),
    ));
    let mut session = open(&client).await;
    assert!(matches!(
        session.request_body(Some(Bytes::from("fine")), false).await,
        BodyVerdict::Forward(Some(_))
    ));
    match session
        .request_body(Some(Bytes::from("a secret")), false)
        .await
    {
        BodyVerdict::Respond(answer) => {
            assert_eq!(answer.status, 403);
            assert_eq!(answer.body, "blocked");
            assert_eq!(answer.headers[0].0.as_str(), "x-by");
        }
        other => panic!("expected the processor's answer, got {other:?}"),
    }
}

#[tokio::test]
async fn a_body_answer_for_another_message_fails_under_the_policy() {
    // a response-headers answer to a request body message was not asked for
    let (address, _seen) = serve(processor(|_, _, _| {
        Reply::Answer(empty_headers_answer(false))
    }))
    .await;
    for (fail_open, expect_forward) in [(false, false), (true, true)] {
        let client = ExtProcClient::new(&config(
            &address,
            serde_json::json!({ "requestBody": "streamed", "failOpen": fail_open }),
        ));
        let mut session = open(&client).await;
        let verdict = session.request_body(Some(Bytes::from("x")), false).await;
        match (verdict, expect_forward) {
            (BodyVerdict::Forward(Some(b)), true) => {
                assert_eq!(b, "x", "failing open forwards the chunk as it was")
            }
            (BodyVerdict::Respond(a), false) => assert_eq!(a.status, 503),
            (other, _) => panic!("failOpen {fail_open}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_processor_that_stops_answering_a_body_is_cut_off_and_the_entry_stops_asking() {
    let (address, seen) = serve(processor(|_, _, _| Reply::Nothing)).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestBody": "streamed", "timeoutMs": 100, "failOpen": true }),
    ));
    let mut session = open(&client).await;
    assert_eq!(
        session.request_body(Some(Bytes::from("a")), false).await,
        BodyVerdict::Forward(Some(Bytes::from("a")))
    );
    assert_eq!(
        session.request_body(Some(Bytes::from("b")), true).await,
        BodyVerdict::Forward(Some(Bytes::from("b")))
    );
    assert_eq!(
        kinds(&seen),
        ["request-headers", "request-body"],
        "after the failure nothing more is sent on the stream"
    );
}

#[tokio::test]
async fn the_response_headers_phase_changes_the_headers_or_replaces_the_response() {
    use processing_request::Request as R;
    let (address, seen) = serve(script(|m| match &m.request {
        Some(R::RequestHeaders(_)) => Reply::Answer(empty_headers_answer(true)),
        Some(R::ResponseHeaders(h)) => {
            let status = h
                .headers
                .as_ref()
                .and_then(|map| map.headers.iter().find(|v| v.key == ":status"))
                .map(|v| String::from_utf8_lossy(&v.raw_value).into_owned())
                .unwrap_or_default();
            if status == "500" {
                Reply::Answer(immediate(503, "masked"))
            } else {
                Reply::Answer(ProcessingResponse {
                    response: Some(processing_response::Response::ResponseHeaders(
                        PhaseResponse {
                            response: Some(CommonResponse {
                                status: 0,
                                header_mutation: Some(HeaderMutation {
                                    set_headers: vec![HeaderValueOption {
                                        header: Some(PbValue {
                                            key: "x-seen".into(),
                                            value: String::new(),
                                            raw_value: b"yes".to_vec(),
                                        }),
                                        append: None,
                                        append_action: 2,
                                        keep_empty_value: false,
                                    }],
                                    remove_headers: vec!["x-secret".into()],
                                }),
                                body_mutation: None,
                            }),
                        },
                    )),
                })
            }
        }
        _ => Reply::Nothing,
    }))
    .await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "responsePhase": true }),
    ));
    let sessions = ExtProcSessions::new();
    sessions.push(open(&client).await).await;
    assert!(sessions.handles_response());
    assert!(!sessions.rewrites_request_body());
    assert_eq!(sessions.len().await, 1);

    let mut headers = HeaderMap::new();
    headers.insert("x-secret", HeaderValue::from_static("1"));
    let verdict = sessions
        .response_headers(
            0,
            &ResponseInfo {
                status: 200,
                headers: &headers,
                end_of_stream: false,
            },
        )
        .await;
    let Verdict::Continue(changes) = verdict else {
        panic!("expected changes, got {verdict:?}");
    };
    assert_eq!(changes.set[0].name.as_str(), "x-seen");
    assert_eq!(changes.remove[0].as_str(), "x-secret");

    // a second response on a new stream, for a 500, is replaced
    let sessions = ExtProcSessions::new();
    sessions.push(open(&client).await).await;
    let verdict = sessions
        .response_headers(
            0,
            &ResponseInfo {
                status: 500,
                headers: &headers,
                end_of_stream: true,
            },
        )
        .await;
    assert!(
        matches!(verdict, Verdict::Respond(ref a) if a.status == 503 && a.body == "masked"),
        "{verdict:?}"
    );
    assert_eq!(
        kinds(&seen)
            .iter()
            .filter(|k| **k == "response-headers")
            .count(),
        2
    );

    // an index with no session is not an error
    let none = sessions
        .response_headers(
            7,
            &ResponseInfo {
                status: 200,
                headers: &headers,
                end_of_stream: true,
            },
        )
        .await;
    assert!(matches!(none, Verdict::Continue(c) if c.is_empty()));
}

#[tokio::test]
async fn an_entry_without_a_response_phase_is_not_asked_about_the_response() {
    let (address, seen) = serve(processor(|_, _, _| Reply::Nothing)).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestBody": "buffered" }),
    ));
    let sessions = ExtProcSessions::new();
    sessions.push(open(&client).await).await;
    assert!(sessions.rewrites_request_body());
    assert!(!sessions.handles_response());
    let headers = HeaderMap::new();
    let verdict = sessions
        .response_headers(
            0,
            &ResponseInfo {
                status: 200,
                headers: &headers,
                end_of_stream: false,
            },
        )
        .await;
    assert!(matches!(verdict, Verdict::Continue(c) if c.is_empty()));
    assert_eq!(kinds(&seen), ["request-headers"]);
}

#[tokio::test]
async fn the_request_headers_are_not_sent_when_request_phase_is_off() {
    let (address, seen) = serve(processor(|_, _, _| Reply::Answer(body_answer(true, None)))).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestPhase": false, "requestBody": "streamed" }),
    ));
    let mut session = open(&client).await;
    assert_eq!(
        session.request_body(Some(Bytes::from("a")), true).await,
        BodyVerdict::Forward(Some(Bytes::from("a")))
    );
    assert_eq!(
        kinds(&seen),
        ["request-body"],
        "the stream opens with the first body"
    );
}

#[tokio::test]
async fn an_observed_response_body_is_copied_to_the_processor_without_waiting_for_it() {
    // the processor answers nothing: the proxy must not wait for it
    let (address, seen) = serve(processor(|_, _, _| Reply::Nothing)).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "responseBody": "observe" }),
    ));
    let sessions = ExtProcSessions::new();
    sessions.push(open(&client).await).await;
    assert!(sessions.handles_response());

    sessions.start_observing().await;
    let started = std::time::Instant::now();
    sessions.observe_response_body(&Bytes::from("one"), false);
    sessions.observe_response_body(&Bytes::from("two"), false);
    sessions.observe_response_body(&Bytes::new(), true);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "observing must not wait for the processor"
    );

    for _ in 0..100 {
        if bodies(&seen, false).len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        bodies(&seen, false),
        [
            ("one".to_owned(), false),
            ("two".to_owned(), false),
            (String::new(), true)
        ]
    );
}

#[tokio::test]
async fn an_observed_body_without_a_stream_opens_one_with_its_first_chunk_and_a_dead_processor_is_only_logged(
) {
    let (address, seen) = serve(processor(|_, _, _| Reply::Nothing)).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestPhase": false, "responseBody": "observe" }),
    ));
    let mut session = open(&client).await;
    session.start_observing();
    session.observe_response_body(&Bytes::from("x"), true);
    for _ in 0..100 {
        if !bodies(&seen, false).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(bodies(&seen, false), [("x".to_owned(), true)]);

    // nothing listens: the chunks go nowhere and nothing fails
    let dead = crate::support::dead_address().await;
    let client = ExtProcClient::new(&config(
        &dead,
        serde_json::json!({ "requestPhase": false, "responseBody": "observe", "timeoutMs": 200 }),
    ));
    let mut session = open(&client).await;
    session.start_observing();
    session.observe_response_body(&Bytes::from("y"), true);
    tokio::time::sleep(Duration::from_millis(400)).await;
}

#[tokio::test]
async fn observing_does_nothing_for_an_entry_that_does_not_observe_or_whose_stream_failed() {
    let (address, seen) = serve(processor(|_, _, _| Reply::Nothing)).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestBody": "buffered" }),
    ));
    let mut session = open(&client).await;
    session.start_observing();
    session.observe_response_body(&Bytes::from("z"), true);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(bodies(&seen, false).is_empty());

    // a processor that does not keep up: the queue fills and the entry stops feeding it
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "responseBody": "observe" }),
    ));
    let mut session = open(&client).await;
    session.start_observing();
    for _ in 0..2000 {
        session.observe_response_body(&Bytes::from_static(&[0u8; 64]), false);
    }
}

#[tokio::test]
async fn the_sessions_of_several_entries_are_asked_in_order_and_the_first_refusal_decides() {
    let (first, _) = serve(processor(|body, _, _| {
        Reply::Answer(body_answer(true, Some(&format!("{body}+1"))))
    }))
    .await;
    let (second, _) = serve(processor(|body, _, _| {
        if body.ends_with("+1") {
            Reply::Answer(body_answer(true, Some(&format!("{body}+2"))))
        } else {
            Reply::Answer(immediate(400, "not seen by the first"))
        }
    }))
    .await;
    let sessions = ExtProcSessions::new();
    for address in [&first, &second] {
        let client = ExtProcClient::new(&config(
            address,
            serde_json::json!({ "requestBody": "streamed" }),
        ));
        sessions.push(open(&client).await).await;
    }
    assert!(!sessions.is_empty().await);
    assert_eq!(
        sessions.request_body(Some(Bytes::from("a")), false).await,
        BodyVerdict::Forward(Some(Bytes::from("a+1+2"))),
        "each entry sees what the one before left"
    );

    // the first entry's processor refuses: the second is not asked
    let refusing = Arc::new(Mutex::new(0));
    let counted = Arc::clone(&refusing);
    let (blocker, _) = serve(processor(|_, _, _| Reply::Answer(immediate(403, "no")))).await;
    let (never, _) = serve(processor(move |_, _, _| {
        *counted.lock().unwrap() += 1;
        Reply::Answer(body_answer(true, None))
    }))
    .await;
    let sessions = ExtProcSessions::new();
    for address in [&blocker, &never] {
        let client = ExtProcClient::new(&config(
            address,
            serde_json::json!({ "requestBody": "streamed" }),
        ));
        sessions.push(open(&client).await).await;
    }
    assert!(matches!(
        sessions.request_body(Some(Bytes::from("a")), false).await,
        BodyVerdict::Respond(a) if a.status == 403
    ));
    assert_eq!(*refusing.lock().unwrap(), 0);
}

fn response_info(headers: &HeaderMap) -> ResponseInfo<'_> {
    ResponseInfo {
        status: 200,
        headers,
        end_of_stream: false,
    }
}

#[tokio::test]
async fn a_refusal_of_the_response_phase_is_repeated_for_the_response_of_a_retried_upstream() {
    // The stream ends with the refusal. The upstream is then tried again (a route's `retry` on 5xx sees the 503 of the
    // refusal) and its next response reaches the same session: it must not go to the client unseen.
    use processing_request::Request as R;
    let (address, _) = serve(script(|m| match &m.request {
        Some(R::RequestHeaders(_)) => Reply::Answer(empty_headers_answer(true)),
        Some(R::ResponseHeaders(_)) => Reply::Answer(immediate(503, "refused")),
        _ => Reply::Nothing,
    }))
    .await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "responsePhase": true }),
    ));
    let mut session = open(&client).await;
    let headers = HeaderMap::new();
    for attempt in 0..3 {
        match session.response_headers(&response_info(&headers)).await {
            Verdict::Respond(answer) => assert_eq!(answer.status, 503, "attempt {attempt}"),
            Verdict::Continue(_) => {
                panic!("attempt {attempt}: the second response went through unseen")
            }
        }
    }
}

#[tokio::test]
async fn a_failed_response_phase_stays_closed_for_later_responses_and_open_when_failing_open() {
    use processing_request::Request as R;
    let behaviour = || {
        script(|m| match &m.request {
            Some(R::RequestHeaders(_)) => Reply::Answer(empty_headers_answer(true)),
            Some(R::ResponseHeaders(_)) => Reply::Close,
            _ => Reply::Nothing,
        })
    };
    let headers = HeaderMap::new();

    let (address, _) = serve(behaviour()).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "responsePhase": true }),
    ));
    let mut session = open(&client).await;
    for attempt in 0..2 {
        match session.response_headers(&response_info(&headers)).await {
            Verdict::Respond(answer) => assert_eq!(answer.status, 503, "attempt {attempt}"),
            Verdict::Continue(_) => panic!("attempt {attempt}: fail-closed let a response through"),
        }
    }

    let (address, _) = serve(behaviour()).await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "responsePhase": true, "failOpen": true }),
    ));
    let mut session = open(&client).await;
    for _ in 0..2 {
        assert!(matches!(
            session.response_headers(&response_info(&headers)).await,
            Verdict::Continue(_)
        ));
    }
}

#[tokio::test]
async fn the_response_is_not_observed_while_the_request_body_is_still_being_exchanged() {
    // The upstream answered before the client finished its upload: observing would take the stream away from the body
    // exchanges that are still to come, so it is skipped; the body that follows is exchanged as usual.
    let (address, seen) = serve(processor(|_, _, request| {
        if request {
            Reply::Answer(body_answer(true, None))
        } else {
            Reply::Nothing
        }
    }))
    .await;
    let client = ExtProcClient::new(&config(
        &address,
        serde_json::json!({ "requestBody": "streamed", "responseBody": "observe" }),
    ));
    let mut session = open(&client).await;
    assert!(matches!(
        session.request_body(Some(Bytes::from("a")), false).await,
        BodyVerdict::Forward(Some(_))
    ));
    session.start_observing();
    session.observe_response_body(&Bytes::from("early"), false);
    assert!(
        matches!(
            session.request_body(Some(Bytes::from("b")), true).await,
            BodyVerdict::Forward(Some(_))
        ),
        "the upload is still exchanged after the response started"
    );
    // the upload is done: now the response can be observed
    session.start_observing();
    session.observe_response_body(&Bytes::from("late"), true);
    for _ in 0..100 {
        if !bodies(&seen, false).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(bodies(&seen, false), [("late".to_owned(), true)]);
}
