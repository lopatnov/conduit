use serde::{Deserialize, Serialize};

/// Fault injection configuration — inject artificial errors or delays into
/// a percentage of requests.  Useful for chaos engineering and testing
/// circuit-breaker / retry behaviour without a real failing upstream.
///
/// ```json
/// {
///   "faultInjection": {
///     "abort": { "percent": 5,  "status": 503 },
///     "delay": { "percent": 10, "ms": 200 }
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FaultInjectionConfig {
    /// Abort a percentage of requests with the given HTTP status code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort: Option<FaultAbort>,
    /// Add an artificial delay to a percentage of requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<FaultDelay>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FaultAbort {
    /// Percentage of requests to abort (0–100).
    pub percent: f64,
    /// HTTP status code to return (default: 503).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Response body text (default: "Fault injected").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FaultDelay {
    /// Percentage of requests to delay (0–100).
    pub percent: f64,
    /// Delay in milliseconds.
    pub ms: u64,
}
