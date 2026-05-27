use std::path::Path;
use std::sync::Arc;

use axum::extract::{Multipart, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use serde_json::json;
use tokio::net::TcpListener;

use crate::proxy::service::AppState;

/// Header injected by the proxy to tell the upload server which site's config to use.
pub const SITE_IDX_HEADER: &str = "x-conduit-site-idx";

/// Build the Axum `Router` for the upload server.
///
/// Exposed so that [`crate::upload::UploadService`] can serve it with a
/// graceful-shutdown handle, while tests can drive it directly.
pub fn make_upload_router(state: Arc<AppState>) -> Router {
    Router::new()
        // Accept POST to any path — the actual upload.path matching is done
        // by the Pingora router before the request reaches this server.
        .route("/{*path}", post(upload_handler))
        .with_state(state)
}

/// Start the Axum file-upload server on the given `listener` and serve
/// requests until the process exits.
///
/// `state` is shared with the Pingora proxy so that the server can read the
/// current `upload` configuration from `AppState.config`.
pub async fn run_upload_server(listener: TcpListener, state: Arc<AppState>) {
    axum::serve(listener, make_upload_router(state)).await.ok();
}

async fn upload_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    // Identify which site's upload config to use via the injected header.
    let site_idx: usize = headers
        .get(SITE_IDX_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let cfg = {
        let config = state.config.load();
        match config.sites.get(site_idx).and_then(|s| s.upload.as_ref()) {
            Some(c) => c.clone(),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "no upload config for this site"})),
                )
                    .into_response()
            }
        }
    };

    // Ensure the upload directory exists.
    if let Err(e) = tokio::fs::create_dir_all(&cfg.dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("could not create upload dir: {e}")})),
        )
            .into_response();
    }

    let field_name = cfg.field_name.as_deref().unwrap_or("file");
    let mut uploaded = Vec::new();
    let mut total_bytes: u64 = 0;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("multipart error: {e}")})),
                )
                    .into_response()
            }
        };

        // Only process fields matching the configured field name.
        if field.name().unwrap_or("") != field_name {
            continue;
        }

        // Max-files guard.
        if let Some(max) = cfg.max_files {
            if uploaded.len() >= max {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "too many files"})),
                )
                    .into_response();
            }
        }

        let original_name = field.file_name().unwrap_or("upload").to_owned();
        let content_type_str = field.content_type().map(str::to_owned);

        // MIME-type allowlist check.
        // When an allowlist is configured, a missing Content-Type header is
        // treated as a rejection: an unknown type cannot be verified against
        // the allowlist, so we refuse the upload rather than silently bypass
        // the check.
        if let Some(allowed) = cfg.allowed_mime_types.as_ref() {
            let ct = content_type_str.as_deref().unwrap_or("");
            let ok = !ct.is_empty() && allowed.iter().any(|a| ct.starts_with(a.as_str()));
            if !ok {
                let msg = if ct.is_empty() {
                    "missing Content-Type; upload rejected by mime-type allowlist".to_owned()
                } else {
                    format!("mime type not allowed: {ct}")
                };
                return (
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    Json(json!({"error": msg})),
                )
                    .into_response();
            }
        }

        // Read the entire field into memory.
        let data = match field.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("read error: {e}")})),
                )
                    .into_response()
            }
        };

        // Per-file size limit.
        let file_bytes = data.len() as u64;
        if let Some(max) = cfg.max_file_size_bytes {
            if file_bytes > max {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(json!({"error": "file exceeds maxFileSizeBytes"})),
                )
                    .into_response();
            }
        }

        total_bytes += file_bytes;

        // Total upload size limit.
        if let Some(max) = cfg.max_total_size_bytes {
            if total_bytes > max {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(json!({"error": "upload exceeds maxTotalSizeBytes"})),
                )
                    .into_response();
            }
        }

        // Infer MIME type from filename when the client did not provide one.
        let mime = content_type_str.unwrap_or_else(|| {
            mime_guess::from_path(&original_name)
                .first_or_octet_stream()
                .to_string()
        });

        // Generate a UUID v4 filename, preserving the original extension.
        let ext = Path::new(&original_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let save_name = format!("{}{ext}", uuid::Uuid::new_v4());
        let save_path = Path::new(&cfg.dir).join(&save_name);

        if let Err(e) = tokio::fs::write(&save_path, &data).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("write error: {e}")})),
            )
                .into_response();
        }

        uploaded.push(json!({
            "name":         save_name,
            "originalName": original_name,
            "size":         file_bytes,
            "mimeType":     mime,
        }));
    }

    if uploaded.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "no files uploaded"})),
        )
            .into_response();
    }

    Json(json!({
        "status": "ok",
        "files":  uploaded,
    }))
    .into_response()
}
