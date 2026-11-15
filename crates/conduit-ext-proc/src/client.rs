//! The gRPC client: the channel to a processor, the request-headers exchange, the reading of the processor's answers, and
//! the policy for when an exchange fails. The stream that stays open for the later phases is in [`crate::session`].
//!
//! One `tonic::transport::Channel` per processor address (and CA file) is shared by every request in the process, like the
//! HTTP client of ForwardAuth: the channel connects lazily, multiplexes the exchanges over one HTTP/2 connection and
//! reconnects by itself.
//!
//! An entry that sends only the request headers (the default) uses one short stream per request: one message, then the
//! end of the stream, and the processor answers with one message. An entry with a later phase keeps its stream open in an
//! [`crate::session::ExtProcSession`].

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

use crate::config::ExtProcConfig;
use crate::mutation::{self, HeaderChanges, Limits};
use crate::proto::{self, processing_response, ProcessingRequest, ProcessingResponse};
use crate::wire::request_message;

/// How long a new connection to the processor may take to open. The exchange itself is bounded by `timeoutMs`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How many messages may wait in the queue of an open stream (the request side of it).
pub(crate) const STREAM_QUEUE: usize = 64;

/// A request, as the processor is told about it.
#[derive(Debug)]
pub struct RequestInfo<'a> {
    /// `:method`.
    pub method: &'a str,
    /// `:scheme`: `http` or `https`.
    pub scheme: &'a str,
    /// `:authority`: the `Host` of the request.
    pub authority: &'a str,
    /// `:path`: the path and the query.
    pub path: &'a str,
    /// The request headers.
    pub headers: &'a HeaderMap,
    /// `true` when no body follows the headers.
    pub end_of_stream: bool,
}

/// An answer to send to the client instead of proxying the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectResponse {
    /// The status.
    pub status: u16,
    /// The headers (replacing the defaults of the response).
    pub headers: Vec<(HeaderName, HeaderValue)>,
    /// The body.
    pub body: Bytes,
}

/// What to do with the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Proxy it, after these changes to the headers (maybe none).
    Continue(HeaderChanges),
    /// End it here with this answer.
    Respond(DirectResponse),
}

/// A response, as the processor is told about it.
#[derive(Debug)]
pub struct ResponseInfo<'a> {
    /// `:status`.
    pub status: u16,
    /// The response headers.
    pub headers: &'a HeaderMap,
    /// `true` when no body follows the headers.
    pub end_of_stream: bool,
}

/// The headers phase an answer belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    RequestHeaders,
    ResponseHeaders,
}

/// What an answer to a body message says to do with the chunk.
#[derive(Debug)]
pub(crate) enum BodyAnswer {
    /// Forward the chunk as it was.
    Keep,
    /// Forward these bytes instead (none at all: the chunk is dropped).
    Replace(Bytes),
    /// End the exchange with this answer to the client.
    Respond(DirectResponse),
}

/// The failure for an answer that does not belong to the message the processor was sent.
fn unexpected(answer: Option<&processing_response::Response>) -> Failure {
    use processing_response::Response as R;
    Failure::Unexpected(match answer {
        Some(R::RequestHeaders(_)) => "a request headers answer",
        Some(R::ResponseHeaders(_)) => "a response headers answer",
        Some(R::RequestBody(_)) => "a request body answer",
        Some(R::ResponseBody(_)) => "a response body answer",
        Some(R::RequestTrailers(_) | R::ResponseTrailers(_)) => "a trailers answer",
        Some(R::ImmediateResponse(_)) => "an immediate response",
        None => "an empty answer",
    })
}

/// Why an exchange did not produce a usable answer.
#[derive(Debug)]
pub(crate) enum Failure {
    Unreachable(String),
    TimedOut(Duration),
    Closed,
    Status(String),
    Unexpected(&'static str),
    Rejected(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "the processor cannot be reached: {e}"),
            Self::TimedOut(d) => write!(
                f,
                "the processor did not answer within {} ms",
                d.as_millis()
            ),
            Self::Closed => f.write_str("the processor closed the stream without answering"),
            Self::Status(s) => write!(f, "the processor answered with an error: {s}"),
            Self::Unexpected(what) => {
                write!(f, "the processor sent {what}, which was not asked for")
            }
            Self::Rejected(why) => {
                write!(f, "the processor's answer is not acceptable: {}", tidy(why))
            }
        }
    }
}

/// A client for one processor.
#[derive(Debug, Clone)]
pub struct ExtProcClient {
    /// The shared channel, or why there is none (a bad address, an unreadable `caFile`): every exchange then fails the
    /// way an unreachable processor does, so the entry's `failOpen` policy decides, not a panic or a skipped check.
    pub(crate) channel: Result<Channel, String>,
    pub(crate) config: ExtProcConfig,
}

/// The channels of the process, by `grpc` address and CA file.
///
/// A channel that could not be built (an unreadable `caFile`) is cached as the error too, until the next reload: the file
/// is read once, not once per request under the lock.
static CHANNELS: OnceLock<Mutex<HashMap<String, Result<Channel, String>>>> = OnceLock::new();

/// Forget the shared channels, so that the next request connects again and reads `caFile` again. A configuration reload
/// calls this (a CA file edited in place, or a processor moved, takes effect without a restart).
pub fn clear_channels() {
    if let Some(channels) = CHANNELS.get() {
        channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

fn channel_for(config: &ExtProcConfig) -> Result<Channel, String> {
    let address = config.endpoint()?;
    let key = format!(
        "{}\0{}",
        config.grpc,
        config.ca_file.as_deref().unwrap_or("")
    );
    let mut channels = CHANNELS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(built) = channels.get(&key) {
        return built.clone();
    }
    let built = build_channel(config, &address);
    channels.insert(key, built.clone());
    built
}

fn build_channel(
    config: &ExtProcConfig,
    address: &crate::config::Endpoint,
) -> Result<Channel, String> {
    let scheme = if address.tls { "https" } else { "http" };
    let mut endpoint = Endpoint::from_shared(format!("{scheme}://{}", address.authority))
        .map_err(|e| format!("bad processor address {:?}: {e}", config.grpc))?
        .connect_timeout(CONNECT_TIMEOUT)
        .tcp_nodelay(true);
    if address.tls {
        let tls = match config.ca_file.as_deref() {
            Some(file) => {
                let pem =
                    std::fs::read(file).map_err(|e| format!("cannot read caFile {file:?}: {e}"))?;
                ClientTlsConfig::new().ca_certificate(Certificate::from_pem(pem))
            }
            None => ClientTlsConfig::new().with_webpki_roots(),
        };
        endpoint = endpoint
            .tls_config(tls)
            .map_err(|e| format!("bad TLS settings for {:?}: {e}", config.grpc))?;
    }
    Ok(endpoint.connect_lazy())
}

impl ExtProcClient {
    /// The client for `config`'s processor. Call it inside a Tokio runtime (the channel connects lazily on it). A
    /// configuration problem (a bad address, an unreadable `caFile`) is not an error here: `validate` reports it up front,
    /// and a client that slipped past that fails every exchange under the entry's `failOpen` policy.
    #[must_use]
    pub fn new(config: &ExtProcConfig) -> Self {
        Self {
            channel: channel_for(config),
            config: config.clone(),
        }
    }

    /// Tell the processor about the request headers and say what to do with the request. A failure of the exchange is
    /// resolved by the entry's policy: `failOpen` continues unchanged, otherwise the answer is `statusOnError`.
    pub async fn process_request_headers(&self, info: &RequestInfo<'_>) -> Verdict {
        let outcome =
            match tokio::time::timeout(self.config.timeout(), self.exchange(request_message(info)))
                .await
            {
                Ok(Ok(answer)) => self.verdict_of(answer),
                Ok(Err(failure)) => Err(failure),
                Err(_) => Err(Failure::TimedOut(self.config.timeout())),
            };
        match outcome {
            Ok(verdict) => verdict,
            Err(failure) => self.on_failure(&failure),
        }
    }

    /// The largest answer accepted from a processor: what `maxBodyBytes` lets Conduit send (the processor may answer with a
    /// body of about that size), never below tonic's own 4 MiB default, plus room for headers and framing.
    pub(crate) fn decode_limit(&self) -> usize {
        self.config.max_body_bytes().max(4 * 1024 * 1024) + 64 * 1024
    }

    /// Open a stream that stays open: `first` is queued before the call, so a processor that waits for the first message
    /// before it sends its response headers is not waited on. The sender ends the request side when it is dropped.
    pub(crate) async fn open_stream(
        &self,
        first: ProcessingRequest,
    ) -> Result<
        (
            tokio::sync::mpsc::Sender<ProcessingRequest>,
            tonic::Streaming<ProcessingResponse>,
        ),
        Failure,
    > {
        let channel = self.channel.clone().map_err(Failure::Unreachable)?;
        let (tx, queue) = tokio::sync::mpsc::channel(STREAM_QUEUE);
        tx.try_send(first).map_err(|_| Failure::Closed)?;
        let mut grpc =
            tonic::client::Grpc::new(channel).max_decoding_message_size(self.decode_limit());
        grpc.ready()
            .await
            .map_err(|e| Failure::Unreachable(e.to_string()))?;
        let codec = tonic_prost::ProstCodec::<ProcessingRequest, ProcessingResponse>::default();
        let path = http::uri::PathAndQuery::from_static(proto::PROCESS_PATH);
        let request = tonic::Request::new(tokio_stream::wrappers::ReceiverStream::new(queue));
        let response = grpc
            .streaming(request, path, codec)
            .await
            .map_err(|s| Failure::Status(describe(&s)))?;
        Ok((tx, response.into_inner()))
    }

    async fn exchange(&self, message: ProcessingRequest) -> Result<ProcessingResponse, Failure> {
        let channel = self.channel.clone().map_err(Failure::Unreachable)?;
        let mut grpc =
            tonic::client::Grpc::new(channel).max_decoding_message_size(self.decode_limit());
        grpc.ready()
            .await
            .map_err(|e| Failure::Unreachable(e.to_string()))?;
        let codec = tonic_prost::ProstCodec::<ProcessingRequest, ProcessingResponse>::default();
        let path = http::uri::PathAndQuery::from_static(proto::PROCESS_PATH);
        // one request message, then the end of the stream: the processor answers and is done
        let request = tonic::Request::new(tokio_stream::iter(vec![message]));
        let response = grpc
            .streaming(request, path, codec)
            .await
            .map_err(|s| Failure::Status(describe(&s)))?;
        match response.into_inner().message().await {
            Ok(Some(answer)) => Ok(answer),
            Ok(None) => Err(Failure::Closed),
            Err(status) => Err(Failure::Status(describe(&status))),
        }
    }

    /// The verdict for an answer to the request headers, or why the answer cannot be used.
    fn verdict_of(&self, answer: ProcessingResponse) -> Result<Verdict, Failure> {
        self.headers_verdict(answer, Phase::RequestHeaders)
    }

    /// The limits on what the processor may change in headers.
    pub(crate) fn limits(&self) -> Limits {
        Limits {
            max_mutations: self.config.max_header_mutations(),
            max_value_bytes: self.config.max_header_value_bytes(),
        }
    }

    /// The verdict for an answer to the headers of `phase` (an answer for any other message is a failure), or why the
    /// answer cannot be used.
    pub(crate) fn headers_verdict(
        &self,
        answer: ProcessingResponse,
        phase: Phase,
    ) -> Result<Verdict, Failure> {
        use processing_response::Response as R;
        let limits = self.limits();
        match (answer.response, phase) {
            (Some(R::RequestHeaders(p)), Phase::RequestHeaders)
            | (Some(R::ResponseHeaders(p)), Phase::ResponseHeaders) => {
                let Some(common) = p.response else {
                    return Ok(Verdict::Continue(HeaderChanges::default()));
                };
                if common.body_mutation.is_some() {
                    tracing::warn!(
                        target: "conduit::ext_proc",
                        "the processor sent a body mutation with the headers: it is not applied (a body is changed in the answer to a body message)"
                    );
                }
                let Some(mutation) = common.header_mutation else {
                    return Ok(Verdict::Continue(HeaderChanges::default()));
                };
                let plan = mutation::plan(&mutation, limits)
                    .map_err(|r| Failure::Rejected(r.to_string()))?;
                note_ignored(&plan.ignored);
                Ok(Verdict::Continue(plan.changes))
            }
            (Some(R::ImmediateResponse(immediate)), _) => {
                self.immediate(immediate).map(Verdict::Respond)
            }
            (other, _) => Err(unexpected(other.as_ref())),
        }
    }

    /// What an answer to a body message says to do with the chunk: the same bytes, other bytes, or an answer to the client.
    /// `request` says which body was sent (an answer for the other one is a failure).
    pub(crate) fn body_answer(
        &self,
        answer: ProcessingResponse,
        request: bool,
    ) -> Result<BodyAnswer, Failure> {
        use processing_response::Response as R;
        match answer.response {
            Some(R::RequestBody(p)) if request => Ok(self.body_change(p)),
            Some(R::ResponseBody(p)) if !request => Ok(self.body_change(p)),
            Some(R::ImmediateResponse(immediate)) => {
                self.immediate(immediate).map(BodyAnswer::Respond)
            }
            other => Err(unexpected(other.as_ref())),
        }
    }

    fn body_change(&self, phase: proto::PhaseResponse) -> BodyAnswer {
        let Some(common) = phase.response else {
            return BodyAnswer::Keep;
        };
        if common.header_mutation.is_some() {
            tracing::warn!(
                target: "conduit::ext_proc",
                "the processor sent a header mutation with a body answer: it is not applied (the headers are already on their way)"
            );
        }
        match common.body_mutation.and_then(|m| m.mutation) {
            Some(proto::body_mutation::Mutation::Body(bytes)) => {
                BodyAnswer::Replace(Bytes::from(bytes))
            }
            Some(proto::body_mutation::Mutation::ClearBody(true)) => {
                BodyAnswer::Replace(Bytes::new())
            }
            Some(proto::body_mutation::Mutation::ClearBody(false)) | None => BodyAnswer::Keep,
        }
    }

    /// An `ImmediateResponse` as the answer to send to the client, or why it is not acceptable.
    fn immediate(&self, immediate: proto::ImmediateResponse) -> Result<DirectResponse, Failure> {
        let limits = self.limits();
        let code = immediate.status.map_or(0, |s| s.code);
        let status = u16::try_from(code)
            .ok()
            .filter(|c| (200..=599).contains(c))
            .ok_or_else(|| {
                Failure::Rejected(format!(
                    "immediate response status {code} is not 200 to 599"
                ))
            })?;
        let headers = match immediate.headers {
            Some(mutation) => {
                let plan = mutation::plan(&mutation, limits)
                    .map_err(|r| Failure::Rejected(r.to_string()))?;
                note_ignored(&plan.ignored);
                plan.changes
                    .set
                    .into_iter()
                    .map(|s| (s.name, s.value))
                    .collect()
            }
            None => Vec::new(),
        };
        if !immediate.details.is_empty() {
            tracing::debug!(
                target: "conduit::ext_proc",
                status,
                details = %tidy(&immediate.details),
                "the external processor ended the request"
            );
        }
        Ok(DirectResponse {
            status,
            headers,
            body: Bytes::from(immediate.body),
        })
    }

    /// The entry's policy for a failed exchange.
    pub(crate) fn on_failure(&self, failure: &Failure) -> Verdict {
        if self.config.fail_open() {
            tracing::warn!(
                target: "conduit::ext_proc",
                processor = %self.config.grpc,
                reason = %failure,
                "external processing failed; the request continues unchanged (failOpen)"
            );
            Verdict::Continue(HeaderChanges::default())
        } else {
            tracing::warn!(
                target: "conduit::ext_proc",
                processor = %self.config.grpc,
                reason = %failure,
                status = self.config.status_on_error(),
                "external processing failed; the request is refused"
            );
            Verdict::Respond(DirectResponse {
                status: self.config.status_on_error(),
                headers: Vec::new(),
                body: Bytes::from_static(b"external processing failed\n"),
            })
        }
    }
}

/// Text that came from the processor, made safe for a log line: escaped (no line breaks or control characters reach the
/// log) and cut at 200 characters (a compromised processor could otherwise send megabytes on every request).
pub(crate) fn tidy(text: &str) -> String {
    const MAX: usize = 200;
    let escaped: String = text.escape_debug().take(MAX + 1).collect();
    if escaped.chars().count() > MAX {
        let cut: String = escaped.chars().take(MAX).collect();
        format!("{cut}...")
    } else {
        escaped
    }
}

pub(crate) fn describe(status: &tonic::Status) -> String {
    format!("{:?}: {}", status.code(), tidy(status.message()))
}

fn note_ignored(ignored: &[String]) {
    if !ignored.is_empty() {
        tracing::warn!(
            target: "conduit::ext_proc",
            headers = %tidy(&ignored.join(", ")),
            "the processor tried to change headers it may not change; ignored"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_from_the_processor_is_escaped_and_cut_before_it_reaches_a_log() {
        assert_eq!(tidy("a\r\nFAKE: line"), "a\\r\\nFAKE: line");
        let long = "x".repeat(10_000);
        let cut = tidy(&long);
        assert_eq!(cut.chars().count(), 203);
        assert!(cut.ends_with("..."));
        assert_eq!(tidy("short"), "short");
    }
}
