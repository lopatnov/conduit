//! `ExtProcConfig`: the `config` object of an `ext_proc` middleware entry.
//!
//! ```yaml
//! middleware:
//!   - type: ext_proc
//!     config:
//!       grpc: grpc://filter-service:9000   # or grpcs:// for TLS
//!       timeoutMs: 200                     # per exchange with the processor
//!       failOpen: false                    # default: refuse the request when the processor fails
//!       requestBody: buffered              # none (default) | buffered | streamed
//!       responsePhase: true                # send the response headers too (default false)
//!       responseBody: observe              # none (default) | observe
//! ```
//!
//! Always compiled: see the crate root for why.

use serde::{Deserialize, Serialize};

/// How long one exchange with the processor may take when `timeoutMs` is absent.
pub const DEFAULT_TIMEOUT_MS: u64 = 200;
/// The status of the answer to a request whose processing failed (fail-closed), when `statusOnError` is absent.
pub const DEFAULT_STATUS_ON_ERROR: u16 = 503;
/// How many header mutations (sets plus removals) one answer may carry, when `maxHeaderMutations` is absent.
pub const DEFAULT_MAX_HEADER_MUTATIONS: usize = 64;
/// How many bytes a header value set by the processor may have, when `maxHeaderValueBytes` is absent.
pub const DEFAULT_MAX_HEADER_VALUE_BYTES: usize = 8192;

/// The most bytes of a request body held for a processor in `buffered` mode, when `maxBodyBytes` is absent (1 MiB, like
/// `limits.maxBodyBufferBytes`).
pub const DEFAULT_MAX_BODY_BYTES: usize = 1_048_576;

/// The longest `timeoutMs` accepted (one minute): an exchange that long holds a connection of the proxy.
pub const MAX_TIMEOUT_MS: u64 = 60_000;
/// The largest `maxHeaderMutations` accepted.
pub const MAX_HEADER_MUTATIONS_LIMIT: usize = 1024;
/// The largest `maxHeaderValueBytes` accepted.
pub const MAX_HEADER_VALUE_BYTES_LIMIT: usize = 65_536;
/// The largest `maxBodyBytes` accepted (16 MiB): the body is held in memory once per request that carries one.
pub const MAX_BODY_BYTES_LIMIT: usize = 16 * 1024 * 1024;

/// What is done with the request body.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RequestBodyMode {
    /// The body is not sent to the processor.
    #[default]
    None,
    /// The whole body is held, up to `maxBodyBytes` (more is refused with 413), sent as one message, and replaced by the
    /// processor's answer. The request is forwarded with chunked framing, because its length is not known up front.
    Buffered,
    /// Every chunk is sent as it arrives and replaced by the processor's answer to it (one answer per chunk, in step).
    /// The request is forwarded with chunked framing.
    Streamed,
}

/// What is done with the response body.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ResponseBodyMode {
    /// The body is not sent to the processor.
    #[default]
    None,
    /// Every chunk is copied to the processor without waiting for it; whatever the processor answers about the body is
    /// ignored (Pingora's response body hooks are synchronous, so a remote call cannot hold the response).
    Observe,
}

/// The `config` of one `ext_proc` entry. Unknown keys are rejected: a misspelt option must not be
/// accepted and then silently do nothing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtProcConfig {
    /// The processor: `grpc://host:port` (plain text) or `grpcs://host:port` (TLS).
    pub grpc: String,
    /// Milliseconds one exchange with the processor may take. Absent: [`DEFAULT_TIMEOUT_MS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// `true`: a request continues unchanged when the processor fails. Absent or `false`: it is refused with
    /// `statusOnError` (fail-closed, like Envoy's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_open: Option<bool>,
    /// The status of the refusal when the processor fails and `failOpen` is not set. Absent:
    /// [`DEFAULT_STATUS_ON_ERROR`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_on_error: Option<u16>,
    /// A PEM file with the certificate authority that signed the processor's certificate (`grpcs://` only). Absent: the
    /// bundled public roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<String>,
    /// The most header mutations one answer may carry. Absent: [`DEFAULT_MAX_HEADER_MUTATIONS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_header_mutations: Option<usize>,
    /// The most bytes a header value set by the processor may have. Absent: [`DEFAULT_MAX_HEADER_VALUE_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_header_value_bytes: Option<usize>,
    /// `false`: the request headers are not sent (a processor that wants only bodies, or only the response). Absent:
    /// `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_phase: Option<bool>,
    /// What is done with the request body. Absent: `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBodyMode>,
    /// `true`: the response headers are sent to the processor, which may change them or replace the response. Absent:
    /// `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_phase: Option<bool>,
    /// What is done with the response body. Absent: `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<ResponseBodyMode>,
    /// The most bytes of a request body held in `buffered` mode; a larger body is refused with 413. Absent:
    /// [`DEFAULT_MAX_BODY_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body_bytes: Option<usize>,
}

/// The processor's address, split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// `grpcs://` (TLS).
    pub tls: bool,
    /// `host:port` (the port is mandatory: there is no default one for gRPC).
    pub authority: String,
}

impl ExtProcConfig {
    /// Read the `config` value of an entry. The error text names what is wrong.
    pub fn from_value(value: Option<&serde_json::Value>) -> Result<Self, String> {
        let value = value.ok_or_else(|| {
            "middleware type \"ext_proc\" requires a \"config\" object with at least \"grpc\""
                .to_owned()
        })?;
        serde_json::from_value(value.clone()).map_err(|e| format!("invalid ext_proc config: {e}"))
    }

    /// `true` when the request continues unchanged on a processor failure.
    pub fn fail_open(&self) -> bool {
        self.fail_open.unwrap_or(false)
    }

    /// The exchange timeout.
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS))
    }

    /// The status of the fail-closed refusal.
    pub fn status_on_error(&self) -> u16 {
        self.status_on_error.unwrap_or(DEFAULT_STATUS_ON_ERROR)
    }

    /// The mutation count limit.
    pub fn max_header_mutations(&self) -> usize {
        self.max_header_mutations
            .unwrap_or(DEFAULT_MAX_HEADER_MUTATIONS)
    }

    /// The header value size limit.
    pub fn max_header_value_bytes(&self) -> usize {
        self.max_header_value_bytes
            .unwrap_or(DEFAULT_MAX_HEADER_VALUE_BYTES)
    }

    /// `true` when the request headers are sent.
    pub fn sends_request_headers(&self) -> bool {
        self.request_phase.unwrap_or(true)
    }

    /// What is done with the request body.
    pub fn request_body_mode(&self) -> RequestBodyMode {
        self.request_body.unwrap_or_default()
    }

    /// `true` when the response headers are sent.
    pub fn sends_response_headers(&self) -> bool {
        self.response_phase.unwrap_or(false)
    }

    /// What is done with the response body.
    pub fn response_body_mode(&self) -> ResponseBodyMode {
        self.response_body.unwrap_or_default()
    }

    /// The most bytes of a request body held in `buffered` mode.
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES)
    }

    /// `true` when the processor is told anything after the request headers: the stream then stays open for the whole
    /// request, held in the per-request state, instead of ending after the answer to the headers.
    pub fn needs_session(&self) -> bool {
        self.request_body_mode() != RequestBodyMode::None
            || self.sends_response_headers()
            || self.response_body_mode() != ResponseBodyMode::None
    }

    /// The address split into scheme and authority; the error says what is wrong with `grpc`.
    pub fn endpoint(&self) -> Result<Endpoint, String> {
        let (tls, rest) = if let Some(rest) = self.grpc.strip_prefix("grpcs://") {
            (true, rest)
        } else if let Some(rest) = self.grpc.strip_prefix("grpc://") {
            (false, rest)
        } else {
            return Err(format!(
                "grpc {:?} must start with grpc:// (plain text) or grpcs:// (TLS)",
                self.grpc
            ));
        };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ', '\t', '\r', '\n']) {
            return Err(format!(
                "grpc {:?} must be grpc[s]://host:port, with no path, query, user name or spaces",
                self.grpc
            ));
        }
        // the port is the part after the last ':' that is not inside an IPv6 literal's brackets
        let host_end = if rest.starts_with('[') {
            rest.find(']').map(|i| i + 1)
        } else {
            Some(rest.find(':').unwrap_or(rest.len()))
        };
        let Some(host_end) = host_end else {
            return Err(format!("grpc {:?} has an unterminated [ ]", self.grpc));
        };
        let (host, port) = rest.split_at(host_end);
        if host.is_empty() || host == "[]" {
            return Err(format!("grpc {:?} has no host", self.grpc));
        }
        let ok_port = port
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .is_some_and(|p| p != 0);
        if !ok_port {
            return Err(format!(
                "grpc {:?} needs a port from 1 to 65535 (there is no default for gRPC)",
                self.grpc
            ));
        }
        Ok(Endpoint {
            tls,
            authority: rest.to_owned(),
        })
    }

    /// What is wrong with the configuration, as `(field, message)` pairs, empty when it is usable.
    #[must_use]
    pub fn problems(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let endpoint = self.endpoint();
        if let Err(message) = &endpoint {
            out.push(("grpc", message.clone()));
        }
        if let Some(ms) = self.timeout_ms {
            if ms == 0 || ms > MAX_TIMEOUT_MS {
                out.push((
                    "timeoutMs",
                    format!(
                        "timeoutMs is {ms}, it must be from 1 to {MAX_TIMEOUT_MS} (one minute)"
                    ),
                ));
            }
        }
        if let Some(status) = self.status_on_error {
            if !(400..=599).contains(&status) {
                out.push((
                    "statusOnError",
                    format!(
                        "statusOnError is {status}, it must be an error status from 400 to 599"
                    ),
                ));
            }
        }
        if let Some(file) = &self.ca_file {
            if file.trim().is_empty() {
                out.push(("caFile", "caFile must not be empty".to_owned()));
            } else if endpoint.as_ref().is_ok_and(|e| !e.tls) {
                out.push((
                    "caFile",
                    "caFile only applies to a grpcs:// processor (a grpc:// connection has no TLS)"
                        .to_owned(),
                ));
            }
        }
        if let Some(n) = self.max_header_mutations {
            if n == 0 || n > MAX_HEADER_MUTATIONS_LIMIT {
                out.push((
                    "maxHeaderMutations",
                    format!("maxHeaderMutations is {n}, it must be from 1 to {MAX_HEADER_MUTATIONS_LIMIT}"),
                ));
            }
        }
        if let Some(n) = self.max_header_value_bytes {
            if n == 0 || n > MAX_HEADER_VALUE_BYTES_LIMIT {
                out.push((
                    "maxHeaderValueBytes",
                    format!(
                        "maxHeaderValueBytes is {n}, it must be from 1 to {MAX_HEADER_VALUE_BYTES_LIMIT}"
                    ),
                ));
            }
        }
        if let Some(n) = self.max_body_bytes {
            if n == 0 || n > MAX_BODY_BYTES_LIMIT {
                out.push((
                    "maxBodyBytes",
                    format!(
                        "maxBodyBytes is {n}, it must be from 1 to {MAX_BODY_BYTES_LIMIT} (16 MiB)"
                    ),
                ));
            }
            if self.request_body_mode() != RequestBodyMode::Buffered {
                out.push((
                    "maxBodyBytes",
                    "maxBodyBytes only applies with requestBody: buffered".to_owned(),
                ));
            }
        }
        if !self.sends_request_headers() && !self.needs_session() {
            out.push((
                "requestPhase",
                "requestPhase is false and no other phase is enabled (requestBody, responsePhase, responseBody): the entry would send nothing".to_owned(),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(json: serde_json::Value) -> ExtProcConfig {
        ExtProcConfig::from_value(Some(&json)).expect("parses")
    }

    #[test]
    fn the_defaults_are_closed_with_a_short_timeout() {
        let c = cfg(serde_json::json!({ "grpc": "grpc://filter:9000" }));
        assert!(!c.fail_open());
        assert_eq!(c.timeout(), std::time::Duration::from_millis(200));
        assert_eq!(c.status_on_error(), 503);
        assert_eq!(c.max_header_mutations(), 64);
        assert_eq!(c.max_header_value_bytes(), 8192);
        assert!(c.problems().is_empty());
    }

    #[test]
    fn a_missing_config_and_unknown_keys_are_refused() {
        assert!(ExtProcConfig::from_value(None)
            .unwrap_err()
            .contains("requires a \"config\""));
        // a misspelt option must not be accepted and then silently do nothing
        let err = ExtProcConfig::from_value(Some(
            &serde_json::json!({ "grpc": "grpc://f:1", "responsePhaze": true }),
        ))
        .unwrap_err();
        assert!(err.contains("responsePhaze"), "{err}");
        for (key, bad) in [("requestBody", "all"), ("responseBody", "buffered")] {
            let err = ExtProcConfig::from_value(Some(
                &serde_json::json!({ "grpc": "grpc://f:1", key: bad }),
            ))
            .unwrap_err();
            assert!(err.contains(bad), "{err}");
        }
        assert!(ExtProcConfig::from_value(Some(&serde_json::json!({}))).is_err());
    }

    #[test]
    fn the_address_is_scheme_host_and_a_mandatory_port() {
        let ep = |grpc: &str| cfg(serde_json::json!({ "grpc": grpc })).endpoint();
        assert_eq!(
            ep("grpc://filter:9000").unwrap(),
            Endpoint {
                tls: false,
                authority: "filter:9000".into()
            }
        );
        assert_eq!(
            ep("grpcs://filter.example.com:443/").unwrap(),
            Endpoint {
                tls: true,
                authority: "filter.example.com:443".into()
            }
        );
        assert!(ep("grpc://[::1]:9000").unwrap().authority == "[::1]:9000");
        for bad in [
            "http://filter:9000",
            "filter:9000",
            "grpc://",
            "grpc://filter",
            "grpc://filter:0",
            "grpc://filter:70000",
            "grpc://filter:9000/path",
            "grpc://user@filter:9000",
            "grpc://:9000",
            "grpc://[::1",
            "grpc://fil ter:9000",
        ] {
            assert!(ep(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn out_of_range_values_are_listed_by_field() {
        let one = |extra: serde_json::Value| {
            let mut v = serde_json::json!({ "grpc": "grpcs://f:1" });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            cfg(v)
                .problems()
                .into_iter()
                .map(|(f, _)| f)
                .collect::<Vec<_>>()
        };
        assert_eq!(one(serde_json::json!({ "timeoutMs": 0 })), ["timeoutMs"]);
        assert_eq!(
            one(serde_json::json!({ "timeoutMs": 60_001 })),
            ["timeoutMs"]
        );
        assert!(one(serde_json::json!({ "timeoutMs": 60_000 })).is_empty());
        assert_eq!(
            one(serde_json::json!({ "statusOnError": 200 })),
            ["statusOnError"]
        );
        assert_eq!(
            one(serde_json::json!({ "statusOnError": 600 })),
            ["statusOnError"]
        );
        assert!(one(serde_json::json!({ "statusOnError": 502 })).is_empty());
        assert_eq!(
            one(serde_json::json!({ "maxHeaderMutations": 0 })),
            ["maxHeaderMutations"]
        );
        assert_eq!(
            one(serde_json::json!({ "maxHeaderMutations": 1025 })),
            ["maxHeaderMutations"]
        );
        assert_eq!(
            one(serde_json::json!({ "maxHeaderValueBytes": 65_537 })),
            ["maxHeaderValueBytes"]
        );
        assert_eq!(one(serde_json::json!({ "caFile": " " })), ["caFile"]);
        // a CA file for a plain-text connection is a mistake
        let plain = cfg(serde_json::json!({ "grpc": "grpc://f:1", "caFile": "ca.pem" }));
        assert_eq!(plain.problems().len(), 1);
        assert!(
            cfg(serde_json::json!({ "grpc": "grpcs://f:1", "caFile": "ca.pem" }))
                .problems()
                .is_empty()
        );
    }

    #[test]
    fn the_phases_default_to_the_request_headers_only() {
        let c = cfg(serde_json::json!({ "grpc": "grpc://f:1" }));
        assert!(c.sends_request_headers());
        assert!(!c.sends_response_headers());
        assert_eq!(c.request_body_mode(), RequestBodyMode::None);
        assert_eq!(c.response_body_mode(), ResponseBodyMode::None);
        assert_eq!(c.max_body_bytes(), 1_048_576);
        assert!(
            !c.needs_session(),
            "the stream ends after the answer to the headers"
        );
    }

    #[test]
    fn any_later_phase_keeps_the_stream_open() {
        for extra in [
            serde_json::json!({ "requestBody": "buffered" }),
            serde_json::json!({ "requestBody": "streamed" }),
            serde_json::json!({ "responsePhase": true }),
            serde_json::json!({ "responseBody": "observe" }),
        ] {
            let mut v = serde_json::json!({ "grpc": "grpc://f:1" });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let c = cfg(v);
            assert!(c.needs_session(), "{extra}");
            assert!(c.problems().is_empty(), "{extra}: {:?}", c.problems());
        }
    }

    #[test]
    fn body_limits_and_an_entry_that_sends_nothing_are_listed_by_field() {
        let fields = |v: serde_json::Value| {
            let mut base = serde_json::json!({ "grpc": "grpc://f:1" });
            base.as_object_mut()
                .unwrap()
                .extend(v.as_object().unwrap().clone());
            cfg(base)
                .problems()
                .into_iter()
                .map(|(f, _)| f)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            fields(serde_json::json!({ "requestBody": "buffered", "maxBodyBytes": 0 })),
            ["maxBodyBytes"]
        );
        assert_eq!(
            fields(serde_json::json!({ "requestBody": "buffered", "maxBodyBytes": 16_777_217 })),
            ["maxBodyBytes"]
        );
        assert!(fields(
            serde_json::json!({ "requestBody": "buffered", "maxBodyBytes": 16_777_216 })
        )
        .is_empty());
        // a limit that nothing reads is a mistake, not a silent no-op
        assert_eq!(
            fields(serde_json::json!({ "maxBodyBytes": 1000 })),
            ["maxBodyBytes"]
        );
        assert_eq!(
            fields(serde_json::json!({ "requestBody": "streamed", "maxBodyBytes": 1000 })),
            ["maxBodyBytes"]
        );
        assert_eq!(
            fields(serde_json::json!({ "requestPhase": false })),
            ["requestPhase"]
        );
        assert!(
            fields(serde_json::json!({ "requestPhase": false, "responsePhase": true })).is_empty()
        );
    }

    #[test]
    fn it_round_trips_without_the_absent_keys() {
        let c = cfg(serde_json::json!({ "grpc": "grpc://f:1", "failOpen": true }));
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"grpc":"grpc://f:1","failOpen":true}"#);
    }
}
