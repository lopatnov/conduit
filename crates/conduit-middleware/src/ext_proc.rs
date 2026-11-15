//! What the `ext_proc` entries do outside the request-headers phase of the guard (issue #27): ending a request or a
//! response with the processor's own answer, and applying its header changes to a response. The per-request streams are
//! in [`conduit_ext_proc::ExtProcSessions`]; the runtime calls these from its body and response hooks.

use bytes::Bytes;
use conduit_ext_proc::{
    DirectResponse, ExtProcSessions, HeaderChanges, ResponseInfo, SetMode, Verdict,
};
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;

/// The headers of a processor's immediate response as strings. A value that is not text cannot be passed on as a response
/// header here (the writer takes strings), so it is left out.
#[must_use]
pub fn text_headers(answer: &DirectResponse) -> Vec<(String, String)> {
    answer
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_owned(), v.to_owned()))
        })
        .collect()
}

/// The header of a response that has no body (204, 205, 304): `headers` without a `Content-Length`, which `write_response`
/// would add. A header the writer cannot take is left out, as `write_response` does.
pub fn bodyless_response(status: u16, headers: Vec<(String, String)>) -> Result<ResponseHeader> {
    let mut resp = ResponseHeader::build(status, Some(headers.len()))?;
    for (name, value) in headers {
        let _ = resp.insert_header(name, value);
    }
    Ok(resp)
}

/// End the request with `status` and `body`: `headers` are the headers of the answer (the site's CORS and security
/// headers, then the processor's own, which win by name).
pub async fn write_answer(
    session: &mut Session,
    status: u16,
    body: Bytes,
    headers: &[(String, String)],
) -> Result<()> {
    if matches!(status, 204 | 205 | 304) {
        // These have no body, and a server must not send `Content-Length` with a 204 (RFC 9110 sections 8.6, 15.3.5,
        // 15.3.6, 15.4.5), which `write_response` always does: the header is built here without it.
        let resp = bodyless_response(status, headers.to_vec())?;
        session.write_response_header(Box::new(resp), true).await?;
        Ok(())
    } else {
        conduit_core::handler::response::write_response(
            session,
            status,
            "text/plain",
            body,
            headers,
        )
        .await
    }
}

/// End the request with the processor's `answer`: the site's `extra` headers, then the processor's.
pub async fn write_direct(
    session: &mut Session,
    answer: &DirectResponse,
    extra: &[(String, String)],
) -> Result<()> {
    let mut all = extra.to_vec();
    all.extend(text_headers(answer));
    write_answer(session, answer.status, answer.body.clone(), &all).await
}

/// `true` when a change in `mode` writes a header that is `present` (or not) in the message.
fn writes(mode: SetMode, present: bool) -> bool {
    match mode {
        SetMode::Append | SetMode::Overwrite => true,
        SetMode::AddIfAbsent => !present,
        SetMode::OverwriteIfExists => present,
    }
}

/// Apply a processor's validated header changes to the response.
pub fn apply_response_changes(resp: &mut ResponseHeader, changes: &HeaderChanges) {
    // removals first, as Envoy does: a header that is removed and set again stays set
    for name in &changes.remove {
        resp.remove_header(name);
    }
    for set in &changes.set {
        if !writes(set.mode, resp.headers.contains_key(&set.name)) {
            continue;
        }
        let _ = if set.mode == SetMode::Append {
            resp.append_header(&set.name, set.value.clone()).map(|_| ())
        } else {
            resp.insert_header(&set.name, set.value.clone())
        };
    }
}

/// Apply a processor's validated header changes to the request, and to the lower-cased view later entries read.
pub fn apply_request_changes(
    request: &mut pingora_http::RequestHeader,
    view: &mut std::collections::HashMap<String, String>,
    changes: &HeaderChanges,
) {
    // removals first, as Envoy does: a header that is removed and set again stays set
    for name in &changes.remove {
        request.remove_header(name);
        view.remove(name.as_str());
    }
    for set in &changes.set {
        if !writes(set.mode, request.headers.contains_key(&set.name)) {
            continue;
        }
        let applied = if set.mode == SetMode::Append {
            request.append_header(&set.name, set.value.clone())
        } else {
            request
                .insert_header(&set.name, set.value.clone())
                .map(|()| true)
        };
        if applied.is_err() {
            continue;
        }
        let key = set.name.as_str().to_owned();
        // The view is rebuilt from the request itself after an append: the view the runtime builds at the start keeps
        // one value of a repeated header, so extending it would drop the others.
        let text = if set.mode == SetMode::Append {
            joined_values(request, set.name.as_str())
        } else {
            set.value.to_str().ok().map(str::to_owned)
        };
        match text {
            Some(value) => {
                view.insert(key, value);
            }
            // A value that is not text cannot be put in the view, and the old one is no longer true: later entries must
            // not decide on a stale header, so the header is left out of the view (it is not there for them).
            None => {
                view.remove(&key);
            }
        }
    }
}

/// Every value of header `name` in the request joined with `, `, or `None` when one of them is not text.
fn joined_values(request: &pingora_http::RequestHeader, name: &str) -> Option<String> {
    let mut values = Vec::new();
    for value in request.headers.get_all(name) {
        values.push(value.to_str().ok()?);
    }
    Some(values.join(", "))
}

/// The response-headers phase of every entry that has one, in order, each told the headers as the one before left them.
/// `end_of_stream` is `true` when no body follows. Returns the answer that replaces the response, if an entry sent one
/// (the headers are then whatever the earlier entries left; [`replace_response`] puts the answer in their place).
pub async fn response_headers(
    sessions: &ExtProcSessions,
    resp: &mut ResponseHeader,
    end_of_stream: bool,
) -> Option<DirectResponse> {
    for index in 0..sessions.len().await {
        let verdict = sessions
            .response_headers(
                index,
                &ResponseInfo {
                    status: resp.status.as_u16(),
                    headers: &resp.headers,
                    end_of_stream,
                },
            )
            .await;
        match verdict {
            Verdict::Continue(changes) => apply_response_changes(resp, &changes),
            Verdict::Respond(answer) => return Some(answer),
        }
    }
    None
}

/// Put the processor's `answer` in place of the response: its status and headers replace the upstream's. Returns the
/// body to send in place of the upstream's, or `None` when none can be sent (`body_allowed` is `false`, or the status
/// carries no body).
pub fn replace_response(
    resp: &mut ResponseHeader,
    answer: &DirectResponse,
    body_allowed: bool,
) -> Option<Bytes> {
    let names: Vec<_> = resp.headers.keys().cloned().collect();
    for name in names {
        resp.remove_header(&name);
    }
    let _ = resp.set_status(answer.status);
    for (name, value) in &answer.headers {
        let _ = resp.insert_header(name, value.clone());
    }
    let carries_body = body_allowed && !matches!(answer.status, 204 | 304) && answer.status >= 200;
    if carries_body {
        // A 205 has no content (RFC 9110 section 15.3.6) but, unlike a 204, Pingora would still write the upstream's body
        // after it: it gets an empty one in its place, with `Content-Length: 0`.
        let body = if answer.status == 205 {
            Bytes::new()
        } else {
            answer.body.clone()
        };
        let _ = resp.insert_header("content-length", body.len().to_string());
        Some(body)
    } else {
        if !matches!(answer.status, 204 | 304) && answer.status >= 200 {
            let _ = resp.insert_header("content-length", "0");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_ext_proc::HeaderSet;
    use http::{HeaderName, HeaderValue};
    use pingora_http::RequestHeader;
    use std::collections::HashMap;

    fn set(name: &str, value: &str, mode: SetMode) -> HeaderSet {
        HeaderSet {
            name: HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value: HeaderValue::from_str(value).unwrap(),
            mode,
        }
    }

    fn response() -> ResponseHeader {
        let mut resp = ResponseHeader::build(200, None).unwrap();
        resp.append_header("x-keep", "1").unwrap();
        resp.append_header("x-multi", "a").unwrap();
        resp.append_header("content-length", "5").unwrap();
        resp
    }

    #[test]
    fn response_changes_follow_the_set_modes_and_removals() {
        let mut resp = response();
        let changes = HeaderChanges {
            set: vec![
                set("x-multi", "b", SetMode::Append),
                set("x-keep", "2", SetMode::Overwrite),
                set("x-new", "1", SetMode::AddIfAbsent),
                set("x-ghost", "1", SetMode::OverwriteIfExists),
            ],
            remove: vec![HeaderName::from_static("x-multi")],
        };
        apply_response_changes(&mut resp, &changes);
        assert_eq!(resp.headers.get("x-keep").unwrap(), "2");
        assert_eq!(resp.headers.get("x-new").unwrap(), "1");
        assert!(resp.headers.get("x-ghost").is_none());
        assert_eq!(
            values_of(&resp, "x-multi"),
            ["b"],
            "the removal runs first, so the append that follows stays"
        );
    }

    fn values_of(resp: &ResponseHeader, name: &str) -> Vec<String> {
        resp.headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect()
    }

    fn answer(status: u16, body: &str) -> DirectResponse {
        DirectResponse {
            status,
            headers: vec![(
                HeaderName::from_static("x-by"),
                HeaderValue::from_static("processor"),
            )],
            body: Bytes::from(body.to_owned()),
        }
    }

    #[test]
    fn an_answer_replaces_the_status_the_headers_and_the_body_of_the_response() {
        let mut resp = response();
        let body = replace_response(&mut resp, &answer(403, "no"), true).unwrap();
        assert_eq!(body, "no");
        assert_eq!(resp.status.as_u16(), 403);
        assert_eq!(resp.headers.get("x-by").unwrap(), "processor");
        assert_eq!(resp.headers.get("content-length").unwrap(), "2");
        assert!(
            resp.headers.get("x-keep").is_none(),
            "the upstream's headers are gone"
        );
    }

    #[test]
    fn an_answer_for_a_response_without_a_body_carries_none() {
        let mut resp = response();
        assert!(replace_response(&mut resp, &answer(403, "no"), false).is_none());
        assert_eq!(resp.headers.get("content-length").unwrap(), "0");

        let mut resp = response();
        assert!(replace_response(&mut resp, &answer(204, "ignored"), true).is_none());
        assert!(
            resp.headers.get("content-length").is_none(),
            "a 204 must not carry Content-Length (RFC 9110 section 8.6)"
        );
    }

    #[test]
    fn only_text_values_of_an_answer_become_response_headers() {
        let mut a = answer(403, "");
        a.headers.push((
            HeaderName::from_static("x-binary"),
            HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        ));
        assert_eq!(
            text_headers(&a),
            vec![("x-by".to_owned(), "processor".to_owned())]
        );
    }

    #[test]
    fn a_bodyless_response_has_the_given_headers_and_no_content_length() {
        let headers = vec![
            ("x-site".to_owned(), "a".to_owned()),
            ("bad name".to_owned(), "dropped".to_owned()),
        ];
        let resp = bodyless_response(204, headers).unwrap();
        assert_eq!(resp.status.as_u16(), 204);
        assert_eq!(resp.headers.get("x-site").unwrap(), "a");
        assert!(resp.headers.get("content-length").is_none());
        assert_eq!(
            resp.headers.len(),
            1,
            "a header the writer cannot take is left out"
        );
        assert!(
            bodyless_response(99, Vec::new()).is_err(),
            "a status outside 100 to 999 is an error, not a panic"
        );
    }

    fn request() -> RequestHeader {
        let mut req = RequestHeader::build("GET", b"/", None).unwrap();
        req.append_header("x-keep", "1").unwrap();
        req.append_header("x-multi", "a").unwrap();
        req.append_header("x-multi", "b").unwrap();
        req.append_header("x-user", "ann").unwrap();
        req
    }

    fn values(req: &RequestHeader, name: &str) -> Vec<String> {
        req.headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn every_set_mode_does_what_its_name_says() {
        let mut req = request();
        // the view the runtime builds at the start keeps ONE value of a repeated header (the last one)
        let mut view = HashMap::from([("x-multi".to_owned(), "b".to_owned())]);
        let changes = HeaderChanges {
            set: vec![
                set("x-multi", "c", SetMode::Append),
                set("x-user", "bob", SetMode::Overwrite),
                set("x-user", "carol", SetMode::AddIfAbsent), // present: no change
                set("x-new", "1", SetMode::AddIfAbsent),      // absent: added
                set("x-ghost", "1", SetMode::OverwriteIfExists), // absent: not created
                set("x-keep", "2", SetMode::OverwriteIfExists), // present: replaced
            ],
            remove: Vec::new(),
        };
        apply_request_changes(&mut req, &mut view, &changes);

        assert_eq!(values(&req, "x-multi"), ["a", "b", "c"]);
        assert_eq!(values(&req, "x-user"), ["bob"]);
        assert_eq!(values(&req, "x-new"), ["1"]);
        assert!(req.headers.get("x-ghost").is_none());
        assert_eq!(values(&req, "x-keep"), ["2"]);

        // the view later entries read follows, and an append keeps the values the view had lost
        assert_eq!(view["x-multi"], "a, b, c");
        assert_eq!(view["x-user"], "bob");
        assert_eq!(view["x-new"], "1");
        assert!(!view.contains_key("x-ghost"));
    }

    #[test]
    fn removals_apply_before_the_sets_and_clear_every_value() {
        let mut req = request();
        let mut view = HashMap::from([
            ("x-multi".to_owned(), "a, b".to_owned()),
            ("x-user".to_owned(), "ann".to_owned()),
        ]);
        let changes = HeaderChanges {
            set: vec![set("x-user", "bob", SetMode::Overwrite)],
            remove: vec![
                HeaderName::from_static("x-user"),
                HeaderName::from_static("x-multi"),
            ],
        };
        apply_request_changes(&mut req, &mut view, &changes);
        assert_eq!(
            values(&req, "x-user"),
            ["bob"],
            "a header that is removed and set stays set (Envoy removes first)"
        );
        assert_eq!(view["x-user"], "bob");
        assert!(req.headers.get("x-multi").is_none());
        assert!(!view.contains_key("x-multi"));
        assert_eq!(values(&req, "x-keep"), ["1"]);
    }

    #[test]
    fn a_header_overwritten_with_a_non_text_value_leaves_the_view_so_later_entries_do_not_read_a_stale_one(
    ) {
        let mut req = request();
        let mut view = HashMap::from([("x-user".to_owned(), "ann".to_owned())]);
        let binary = HeaderSet {
            name: HeaderName::from_static("x-user"),
            value: HeaderValue::from_bytes(b"\xff\xfe").unwrap(),
            mode: SetMode::Overwrite,
        };
        let changes = HeaderChanges {
            set: vec![binary],
            remove: Vec::new(),
        };
        apply_request_changes(&mut req, &mut view, &changes);
        assert_eq!(req.headers.get("x-user").unwrap().as_bytes(), b"\xff\xfe");
        assert!(
            !view.contains_key("x-user"),
            "the view must not keep the old value: {view:?}"
        );
    }
}
