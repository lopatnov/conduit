//! The stream of one request, kept open across the phases (issue #27).
//!
//! Envoy's `ext_proc` is one bidirectional stream per HTTP request: the proxy sends the request headers, the request body,
//! the response headers and the response body as messages of that stream, and the processor answers each one. An entry with
//! a later phase (a request body mode, `responsePhase`, `responseBody`) therefore keeps its stream in an
//! [`ExtProcSession`], which lives in the per-request state and is dropped (and the stream with it) when the request ends.
//!
//! One exchange at a time, in step: a message is sent and its answer is awaited, under `timeoutMs`. The one exception is
//! the response body in `observe` mode, whose chunks are copied to the processor by a task without waiting for it (the
//! hooks that see the response body are synchronous in Pingora, so a remote call cannot hold the response there).
//!
//! A failed exchange ends the stream. Under `failOpen` the entry then does nothing for the rest of the request (the
//! request continues as if the entry were not there); otherwise the request is refused with `statusOnError`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use tokio::sync::{mpsc, Mutex};

use crate::client::{
    BodyAnswer, DirectResponse, ExtProcClient, Failure, Phase, RequestInfo, ResponseInfo, Verdict,
};
use crate::config::{RequestBodyMode, ResponseBodyMode};
use crate::mutation::HeaderChanges;
use crate::proto::{ProcessingRequest, ProcessingResponse};
use crate::wire::{body_message, request_message, response_message};

/// How many chunks of the response body may wait for the task that copies them to the processor.
const OBSERVE_QUEUE: usize = 256;
/// How long the answers of an observed response body are still read after the last chunk was sent.
const OBSERVE_DRAIN: Duration = Duration::from_secs(5);

/// What to do with a chunk of the request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyVerdict {
    /// Forward this in place of the chunk that was given: `None` forwards nothing (the chunk is held back, or dropped).
    Forward(Option<Bytes>),
    /// Refuse the request with this answer.
    Respond(DirectResponse),
}

enum Link {
    /// No stream yet: the first message opens it.
    Idle,
    Open {
        tx: mpsc::Sender<ProcessingRequest>,
        // boxed: a `tonic::Streaming` is large, and most links are not open
        rx: Box<tonic::Streaming<ProcessingResponse>>,
    },
    /// The response body is copied to the processor by a task; this is its queue.
    Observing(mpsc::Sender<(Bytes, bool)>),
    /// The stream failed or ended: nothing more is sent on it.
    Dead,
}

/// The stream of one `ext_proc` entry for one request.
pub struct ExtProcSession {
    client: ExtProcClient,
    link: Link,
    /// The request body held so far (`buffered` mode).
    held: BytesMut,
    /// What ended the request when the processor answered with a response (or failed under fail-closed). A later phase of
    /// the same request (the response of a retried upstream) is refused with it again: a request that was refused is never
    /// let through because the stream is gone.
    refusal: Option<DirectResponse>,
    /// `true` once no request body is left to exchange (the last chunk was seen, or the entry does not read the body).
    body_done: bool,
}

impl ExtProcClient {
    /// A session for one request: the stream is opened by its first message.
    #[must_use]
    pub fn session(&self) -> ExtProcSession {
        ExtProcSession {
            client: self.clone(),
            link: Link::Idle,
            held: BytesMut::new(),
            refusal: None,
            body_done: self.config.request_body_mode() == RequestBodyMode::None,
        }
    }
}

impl ExtProcSession {
    /// `true` when this entry reads or changes the request body.
    #[must_use]
    pub fn handles_request_body(&self) -> bool {
        self.client.config.request_body_mode() != RequestBodyMode::None
    }

    /// `true` when this entry has anything to do once the response arrives.
    #[must_use]
    pub fn handles_response(&self) -> bool {
        self.client.config.sends_response_headers()
            || self.client.config.response_body_mode() != ResponseBodyMode::None
    }

    fn ended(&self) -> bool {
        matches!(self.link, Link::Dead)
    }

    /// One exchange: send `message` (opening the stream if need be) and wait for the answer, within `timeoutMs`. Any
    /// failure ends the stream.
    async fn exchange(
        &mut self,
        message: ProcessingRequest,
    ) -> Result<ProcessingResponse, Failure> {
        let timeout = self.client.config.timeout();
        let result = match tokio::time::timeout(timeout, self.exchange_untimed(message)).await {
            Ok(result) => result,
            Err(_) => Err(Failure::TimedOut(timeout)),
        };
        if result.is_err() {
            self.link = Link::Dead;
        }
        result
    }

    async fn exchange_untimed(
        &mut self,
        message: ProcessingRequest,
    ) -> Result<ProcessingResponse, Failure> {
        match &self.link {
            Link::Idle => {
                let (tx, rx) = self.client.open_stream(message).await?;
                self.link = Link::Open {
                    tx,
                    rx: Box::new(rx),
                };
            }
            Link::Open { tx, .. } => tx.send(message).await.map_err(|_| Failure::Closed)?,
            Link::Observing(_) | Link::Dead => return Err(Failure::Closed),
        }
        let Link::Open { rx, .. } = &mut self.link else {
            return Err(Failure::Closed);
        };
        match rx.message().await {
            Ok(Some(answer)) => Ok(answer),
            Ok(None) => Err(Failure::Closed),
            Err(status) => Err(Failure::Status(crate::client::describe(&status))),
        }
    }

    /// What a header verdict (or a failure) means for the stream: an answer that ends the request ends the stream too.
    fn headers_outcome(&mut self, outcome: Result<Verdict, Failure>) -> Verdict {
        let verdict = match outcome {
            Ok(verdict) => {
                if matches!(verdict, Verdict::Respond(_)) {
                    self.link = Link::Dead;
                }
                verdict
            }
            Err(failure) => {
                self.link = Link::Dead;
                self.client.on_failure(&failure)
            }
        };
        if let Verdict::Respond(response) = &verdict {
            self.refusal = Some(response.clone());
        }
        verdict
    }

    /// What a phase answers once the stream has ended: the refusal again when there was one (fail-closed stays closed for
    /// every later response of the request), otherwise nothing to do.
    fn after_end(&self) -> Verdict {
        match &self.refusal {
            Some(response) => Verdict::Respond(response.clone()),
            None => Verdict::Continue(HeaderChanges::default()),
        }
    }

    /// The request headers phase. Not sent (the verdict is "continue unchanged") when the entry has `requestPhase: false`.
    pub async fn request_headers(&mut self, info: &RequestInfo<'_>) -> Verdict {
        if self.ended() {
            return self.after_end();
        }
        if !self.client.config.sends_request_headers() {
            return Verdict::Continue(HeaderChanges::default());
        }
        let outcome = match self.exchange(request_message(info)).await {
            Ok(answer) => self.client.headers_verdict(answer, Phase::RequestHeaders),
            Err(failure) => Err(failure),
        };
        self.headers_outcome(outcome)
    }

    /// The response headers phase (`responsePhase: true`): the processor may change the headers or replace the response.
    pub async fn response_headers(&mut self, info: &ResponseInfo<'_>) -> Verdict {
        if !self.client.config.sends_response_headers() {
            return Verdict::Continue(HeaderChanges::default());
        }
        if self.ended() {
            return self.after_end();
        }
        let outcome = match self.exchange(response_message(info)).await {
            Ok(answer) => self.client.headers_verdict(answer, Phase::ResponseHeaders),
            Err(failure) => Err(failure),
        };
        self.headers_outcome(outcome)
    }

    /// One chunk of the request body (`chunk` is `None` for the end of a body that had no last chunk), and whether it is
    /// the last. What to forward in its place: see [`BodyVerdict`].
    pub async fn request_body(&mut self, chunk: Option<Bytes>, end: bool) -> BodyVerdict {
        self.body_done |= end;
        match self.client.config.request_body_mode() {
            _ if self.ended() => BodyVerdict::Forward(chunk),
            RequestBodyMode::None => BodyVerdict::Forward(chunk),
            RequestBodyMode::Buffered => self.buffered_body(chunk, end).await,
            RequestBodyMode::Streamed => self.streamed_body(chunk, end).await,
        }
    }

    /// No request body is left to exchange (the request has none, and its method carries none): the response may be
    /// observed.
    pub fn request_body_is_done(&mut self) {
        self.body_done = true;
    }

    /// `buffered`: hold the chunks (up to `maxBodyBytes`), send the whole body at the end, forward the answer.
    async fn buffered_body(&mut self, chunk: Option<Bytes>, end: bool) -> BodyVerdict {
        if let Some(chunk) = &chunk {
            if self.held.len() + chunk.len() > self.client.config.max_body_bytes() {
                self.link = Link::Dead;
                self.held = BytesMut::new();
                return BodyVerdict::Respond(DirectResponse {
                    status: 413,
                    headers: Vec::new(),
                    body: Bytes::from_static(
                        b"request body is larger than the external processor's maxBodyBytes\n",
                    ),
                });
            }
            self.held.extend_from_slice(chunk);
        }
        if !end {
            return BodyVerdict::Forward(None);
        }
        let whole = std::mem::take(&mut self.held).freeze();
        let answer = match self.exchange(body_message(&whole, true, true)).await {
            Ok(answer) => self.client.body_answer(answer, true),
            Err(failure) => Err(failure),
        };
        self.body_outcome(answer, Some(whole))
    }

    /// `streamed`: every chunk is sent and the answer to it replaces it.
    async fn streamed_body(&mut self, chunk: Option<Bytes>, end: bool) -> BodyVerdict {
        if !end && chunk.as_ref().is_none_or(Bytes::is_empty) {
            return BodyVerdict::Forward(chunk);
        }
        let data = chunk.clone().unwrap_or_default();
        let answer = match self.exchange(body_message(&data, end, true)).await {
            Ok(answer) => self.client.body_answer(answer, true),
            Err(failure) => Err(failure),
        };
        self.body_outcome(answer, chunk)
    }

    /// The verdict for the answer to a body message; `original` is what was sent (what failing open forwards).
    fn body_outcome(
        &mut self,
        answer: Result<BodyAnswer, Failure>,
        original: Option<Bytes>,
    ) -> BodyVerdict {
        match answer {
            Ok(BodyAnswer::Keep) => BodyVerdict::Forward(original.filter(|b| !b.is_empty())),
            Ok(BodyAnswer::Replace(bytes)) => {
                BodyVerdict::Forward(Some(bytes).filter(|b| !b.is_empty()))
            }
            Ok(BodyAnswer::Respond(response)) => {
                self.link = Link::Dead;
                self.refusal = Some(response.clone());
                BodyVerdict::Respond(response)
            }
            Err(failure) => {
                self.link = Link::Dead;
                match self.client.on_failure(&failure) {
                    Verdict::Continue(_) => {
                        BodyVerdict::Forward(original.filter(|b| !b.is_empty()))
                    }
                    Verdict::Respond(response) => {
                        self.refusal = Some(response.clone());
                        BodyVerdict::Respond(response)
                    }
                }
            }
        }
    }

    /// Start copying the response body to the processor (`responseBody: observe`): call it once, where the response
    /// headers are seen and an `await` is possible. The chunks are then passed on with [`Self::observe_response_body`],
    /// which never waits.
    pub fn start_observing(&mut self) {
        if self.client.config.response_body_mode() != ResponseBodyMode::Observe || self.ended() {
            return;
        }
        if !self.body_done {
            // The upstream answered before the client finished sending its body, so the stream is still needed for the
            // exchanges of the request body: observing would take it away. Observation never fails a request; it is skipped.
            tracing::debug!(
                target: "conduit::ext_proc",
                processor = %self.client.config.grpc,
                "the response is not observed: the request body is still being exchanged"
            );
            return;
        }
        let (queue, chunks) = mpsc::channel(OBSERVE_QUEUE);
        let stream = match std::mem::replace(&mut self.link, Link::Observing(queue)) {
            Link::Open { tx, rx } => Some((tx, *rx)),
            _ => None,
        };
        tokio::spawn(pump(self.client.clone(), stream, chunks));
    }

    /// Copy a chunk of the response body to the processor, without waiting for it. A processor that cannot keep up (the
    /// queue is full) stops being fed: a gap in what it was told would be worse than the end of the observation.
    pub fn observe_response_body(&mut self, chunk: &Bytes, end_of_stream: bool) {
        let Link::Observing(queue) = &self.link else {
            return;
        };
        match queue.try_send((chunk.clone(), end_of_stream)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    target: "conduit::ext_proc",
                    processor = %self.client.config.grpc,
                    "the processor does not keep up with the response body; it is not sent any more of it"
                );
                self.link = Link::Dead;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => self.link = Link::Dead,
        }
    }
}

/// The task behind an observed response body: send what arrives on `chunks`, read and drop the answers.
async fn pump(
    client: ExtProcClient,
    stream: Option<(
        mpsc::Sender<ProcessingRequest>,
        tonic::Streaming<ProcessingResponse>,
    )>,
    mut chunks: mpsc::Receiver<(Bytes, bool)>,
) {
    let (mut tx, mut rx) = match stream {
        Some((tx, rx)) => (Some(tx), Some(rx)),
        None => (None, None),
    };
    loop {
        tokio::select! {
            next = chunks.recv() => {
                let Some((chunk, end)) = next else { break };
                let message = body_message(&chunk, end, false);
                if let (Some(sender), Some(_)) = (&tx, &rx) {
                    // A processor that stopped reading fills the queues of the stream; the send is bounded like every other
                    // exchange, so the task (and the chunks it holds) does not outlive the request for good.
                    match tokio::time::timeout(client.config.timeout(), sender.send(message)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => break,
                        Err(_) => {
                            log_observe_failure(&client, &Failure::TimedOut(client.config.timeout()));
                            return;
                        }
                    }
                } else {
                    match tokio::time::timeout(client.config.timeout(), client.open_stream(message)).await {
                        Ok(Ok((sender, stream))) => {
                            tx = Some(sender);
                            rx = Some(stream);
                        }
                        Ok(Err(failure)) => {
                            log_observe_failure(&client, &failure);
                            return;
                        }
                        Err(_) => {
                            log_observe_failure(&client, &Failure::TimedOut(client.config.timeout()));
                            return;
                        }
                    }
                }
                if end {
                    break;
                }
            }
            answer = async {
                match rx.as_mut() {
                    Some(stream) => stream.message().await,
                    None => std::future::pending().await,
                }
            } => {
                // the answers about an observed body are not acted on
                if !matches!(answer, Ok(Some(_))) {
                    return;
                }
            }
        }
    }
    // the request side ends here; the answers still in flight are read for a moment so that the processor can finish
    drop(tx);
    if let Some(mut stream) = rx {
        let _ = tokio::time::timeout(OBSERVE_DRAIN, async {
            while let Ok(Some(_)) = stream.message().await {}
        })
        .await;
    }
}

fn log_observe_failure(client: &ExtProcClient, failure: &Failure) {
    tracing::warn!(
        target: "conduit::ext_proc",
        processor = %client.config.grpc,
        reason = %failure,
        "the response body is not sent to the processor"
    );
}

/// The sessions of the `ext_proc` entries of one request, in the order of the entries. Created for a request only when
/// its site has an `ext_proc` entry; the request guard fills it, the body and response hooks use it.
#[derive(Default)]
pub struct ExtProcSessions {
    inner: Mutex<Vec<ExtProcSession>>,
    rewrites_request_body: AtomicBool,
    handles_response: AtomicBool,
}

impl std::fmt::Debug for ExtProcSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtProcSessions")
            .field("rewrites_request_body", &self.rewrites_request_body())
            .field("handles_response", &self.handles_response())
            .finish_non_exhaustive()
    }
}

impl ExtProcSessions {
    /// A new, empty set, to share between the guard and the per-request state.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Add the session of an entry (after its request headers phase).
    pub async fn push(&self, session: ExtProcSession) {
        if session.handles_request_body() {
            self.rewrites_request_body.store(true, Ordering::Relaxed);
        }
        if session.handles_response() {
            self.handles_response.store(true, Ordering::Relaxed);
        }
        self.inner.lock().await.push(session);
    }

    /// `true` when an entry reads or changes the request body: the request is then forwarded with chunked framing, since
    /// the length of what is forwarded is no longer the length of what the client sent.
    #[must_use]
    pub fn rewrites_request_body(&self) -> bool {
        self.rewrites_request_body.load(Ordering::Relaxed)
    }

    /// `true` when an entry has anything to do once the response arrives.
    #[must_use]
    pub fn handles_response(&self) -> bool {
        self.handles_response.load(Ordering::Relaxed)
    }

    /// How many sessions there are.
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// `true` when there are none.
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }

    /// A chunk of the request body through every entry that handles it, in order. The first entry that ends the request
    /// decides.
    pub async fn request_body(&self, chunk: Option<Bytes>, end: bool) -> BodyVerdict {
        let mut sessions = self.inner.lock().await;
        let mut current = chunk;
        for session in sessions.iter_mut().filter(|s| s.handles_request_body()) {
            match session.request_body(current.clone(), end).await {
                BodyVerdict::Forward(next) => current = next,
                respond @ BodyVerdict::Respond(_) => return respond,
            }
        }
        BodyVerdict::Forward(current)
    }

    /// The response headers phase of the entry at `index` (the entries are asked one after the other, each told the headers
    /// as the one before left them). A verdict to continue with no changes for an entry that has no such phase.
    pub async fn response_headers(&self, index: usize, info: &ResponseInfo<'_>) -> Verdict {
        let mut sessions = self.inner.lock().await;
        match sessions.get_mut(index) {
            Some(session) => session.response_headers(info).await,
            None => Verdict::Continue(HeaderChanges::default()),
        }
    }

    /// No request body is left to exchange for any entry: see [`ExtProcSession::request_body_is_done`].
    pub async fn request_body_is_done(&self) {
        for session in self.inner.lock().await.iter_mut() {
            session.request_body_is_done();
        }
    }

    /// Start copying the response body to the entries that observe it (call it where the response headers are seen).
    pub async fn start_observing(&self) {
        let mut sessions = self.inner.lock().await;
        for session in sessions.iter_mut() {
            session.start_observing();
        }
    }

    /// Pass a chunk of the response body to the entries that observe it. It never waits.
    pub fn observe_response_body(&self, chunk: &Bytes, end_of_stream: bool) {
        if let Ok(mut sessions) = self.inner.try_lock() {
            for session in sessions.iter_mut() {
                session.observe_response_body(chunk, end_of_stream);
            }
        }
    }
}
