use serde::{Deserialize, Serialize};

/// External authentication service integration.
///
/// The request is forwarded to the auth URL before reaching the upstream.
/// The auth service communicates its decision via HTTP status:
/// - 2xx → allow; copy `responseHeaders` to upstream request
/// - 4xx / 5xx → deny; return the auth service's status to the client
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ForwardAuthConfig {
    /// URL of the authentication/authorization service.
    pub url: String,
    /// Request headers to forward to the auth service.
    ///
    /// When absent or empty, only `X-Forwarded-For`, `X-Forwarded-Method`,
    /// and `X-Forwarded-Uri` are sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Vec<String>>,
    /// Auth service response headers to inject into the upstream request.
    ///
    /// For example: `["X-User-ID", "X-Role"]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Vec<String>>,
    /// Maximum time to wait for the auth service in milliseconds.
    /// Default: 5000 ms.
    #[serde(rename = "timeoutMs", skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Paths that bypass forward-auth entirely (same glob syntax as `skipPaths`).
    #[serde(rename = "skipPaths", skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
}
