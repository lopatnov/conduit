//! Access-log config: `LoggingConfig`, `LogFormat`, `LoggingOptions`.

use serde::{Deserialize, Serialize};

// ── Logging ────────────────────────────────────────────────────────────────

/// `false` | `true` | `"dev"` | `{ "format": "json", "file": "..." }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum LoggingConfig {
    Enabled(bool),
    Format(LogFormat),
    Options(LoggingOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Combined,
    Common,
    Dev,
    Short,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct LoggingOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<LogFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Paths to exclude from access logs.
    ///
    /// Requests whose path matches any entry are silently skipped — useful to
    /// suppress noisy health-check and metrics traffic from access logs.
    /// Supports exact paths and `/**` glob suffixes.
    ///
    /// ```json
    /// { "format": "json", "skipPaths": ["/__health__", "/__metrics__"] }
    /// ```
    #[serde(rename = "skipPaths", skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
    /// Strip the query string from logged request paths.
    ///
    /// When `true`, the access log records only the path component (e.g.
    /// `/api/login`) rather than the full `path?query` string.  This prevents
    /// API tokens or session IDs passed as query parameters from appearing in
    /// plaintext log files.
    ///
    /// Default: `false` (query string is logged, matching standard access-log
    /// behaviour).
    ///
    /// ```json
    /// { "format": "json", "stripQuery": true }
    /// ```
    #[serde(rename = "stripQuery", skip_serializing_if = "Option::is_none")]
    pub strip_query: Option<bool>,
}
