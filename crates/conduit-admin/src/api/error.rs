//! Admin API errors: [`AdminError`], its JSON response and the handlers' [`AdminResult`].

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

// ── Typed error responses ─────────────────────────────────────────────────────

/// Typed error for Admin API handlers.
///
/// Implements [`IntoResponse`] so handlers can return `Result<T, AdminError>`
/// and get consistent JSON error bodies without ad-hoc `json!({ "status": "error" })`.
#[derive(Debug)]
pub enum AdminError {
    /// 400 Bad Request — invalid input (e.g. bad URL format, missing field).
    BadRequest(String),
    /// 500 Internal Server Error — config parse / validation failure.
    ServerError(String),
    /// 400 — reload rejected because cold fields changed; includes the list of
    /// fields as a top-level JSON array so callers can inspect them without
    /// parsing the human-readable `message` string.
    ColdFieldsChanged {
        message: String,
        fields: Vec<String>,
    },
    /// 501 Not Implemented -- the endpoint exists but this build does not
    /// include the feature behind it (e.g. `/cache/purge` without `cache`,
    /// issue #144 PR 4b). Only constructed in such builds.
    #[cfg_attr(feature = "cache", allow(dead_code))]
    NotImplemented(String),
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        match self {
            AdminError::BadRequest(m) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "status": "error", "message": m })),
            )
                .into_response(),
            AdminError::ServerError(m) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error", "message": m })),
            )
                .into_response(),
            AdminError::NotImplemented(m) => (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({ "status": "error", "message": m })),
            )
                .into_response(),
            AdminError::ColdFieldsChanged { message, fields } => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "status":      "error",
                    "message":     message,
                    "cold_fields": fields,
                })),
            )
                .into_response(),
        }
    }
}

/// Shorthand result type for Admin API handlers.
pub type AdminResult<T> = Result<T, AdminError>;
