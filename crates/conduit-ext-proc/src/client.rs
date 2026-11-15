//! The gRPC client: one exchange with the processor per request, and the policy for when it fails.
//!
//! One `tonic::transport::Channel` per processor address (and CA file) is shared by every request in the process, like the
//! HTTP client of ForwardAuth: the channel connects lazily, multiplexes the exchanges over one HTTP/2 connection and
//! reconnects by itself.
//!
//! Slice 1 of issue #27 sends the request headers and nothing else, so the request side of the stream is one message and
//! the stream is half-closed right after it; the processor answers with one message.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

use crate::config::ExtProcConfig;
use crate::mutation::{self, HeaderChanges, Limits};
use crate::proto::{
    self, processing_request, processing_response, HeaderMap as PbHeaderMap, HttpHeaders,
    ProcessingRequest, ProcessingResponse,
};

/// How long a new connection to the processor may take to open. The exchange itself is bounded by `timeoutMs`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Why an exchange did not produce a usable answer.
#[derive(Debug)]
enum Failure {
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
            Self::Rejected(why) => write!(f, "the processor's answer is not acceptable: {why}"),
        }
    }
}

/// A client for one processor.
#[derive(Debug, Clone)]
pub struct ExtProcClient {
    /// The shared channel, or why there is none (a bad address, an unreadable `caFile`): every exchange then fails the
    /// way an unreachable processor does, so the entry's `failOpen` policy decides, not a panic or a skipped check.
    channel: Result<Channel, String>,
    config: ExtProcConfig,
}

/// The channels of the process, by `grpc` address and CA file.
static CHANNELS: OnceLock<Mutex<HashMap<String, Channel>>> = OnceLock::new();

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
    if let Some(channel) = channels.get(&key) {
        return Ok(channel.clone());
    }
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
    let channel = endpoint.connect_lazy();
    channels.insert(key, channel.clone());
    Ok(channel)
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

    async fn exchange(&self, message: ProcessingRequest) -> Result<ProcessingResponse, Failure> {
        let channel = self.channel.clone().map_err(Failure::Unreachable)?;
        let mut grpc = tonic::client::Grpc::new(channel);
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

    /// The verdict for an answer, or why the answer cannot be used.
    fn verdict_of(&self, answer: ProcessingResponse) -> Result<Verdict, Failure> {
        let limits = Limits {
            max_mutations: self.config.max_header_mutations(),
            max_value_bytes: self.config.max_header_value_bytes(),
        };
        match answer.response {
            Some(processing_response::Response::RequestHeaders(phase)) => {
                let Some(common) = phase.response else {
                    return Ok(Verdict::Continue(HeaderChanges::default()));
                };
                if common.body_mutation.is_some() {
                    tracing::warn!(
                        target: "conduit::ext_proc",
                        "the processor sent a body mutation with the request headers: it is not applied (bodies are not sent to it yet)"
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
            Some(processing_response::Response::ImmediateResponse(immediate)) => {
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
                        details = %immediate.details,
                        "the external processor ended the request"
                    );
                }
                Ok(Verdict::Respond(DirectResponse {
                    status,
                    headers,
                    body: Bytes::from(immediate.body),
                }))
            }
            Some(processing_response::Response::ResponseHeaders(_)) => {
                Err(Failure::Unexpected("response headers"))
            }
            Some(processing_response::Response::RequestBody(_)) => {
                Err(Failure::Unexpected("a request body answer"))
            }
            Some(processing_response::Response::ResponseBody(_)) => {
                Err(Failure::Unexpected("a response body answer"))
            }
            Some(
                processing_response::Response::RequestTrailers(_)
                | processing_response::Response::ResponseTrailers(_),
            ) => Err(Failure::Unexpected("a trailers answer")),
            None => Err(Failure::Unexpected("an empty answer")),
        }
    }

    /// The entry's policy for a failed exchange.
    fn on_failure(&self, failure: &Failure) -> Verdict {
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

fn describe(status: &tonic::Status) -> String {
    format!("{:?}: {}", status.code(), status.message())
}

fn note_ignored(ignored: &[String]) {
    if !ignored.is_empty() {
        tracing::warn!(
            target: "conduit::ext_proc",
            headers = %ignored.join(", "),
            "the processor tried to change headers it may not change; ignored"
        );
    }
}

/// The message that tells the processor about the request.
fn request_message(info: &RequestInfo<'_>) -> ProcessingRequest {
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

#[cfg(test)]
mod tests {
    use super::*;

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
