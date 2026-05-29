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
    let site_idx: usize = headers
        .get(SITE_IDX_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let cfg = {
        let config = state.config.load();
        match config.sites.get(site_idx).and_then(|s| s.upload.as_ref()) {
            Some(c) => c.clone(),
            None => return err_response(StatusCode::NOT_FOUND, "no upload config for this site"),
        }
    };

    if let Err(e) = tokio::fs::create_dir_all(&cfg.dir).await {
        return err_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("could not create upload dir: {e}"),
        );
    }

    let field_name = cfg.field_name.as_deref().unwrap_or("file");
    let mut uploaded: Vec<serde_json::Value> = Vec::new();
    let mut total_bytes: u64 = 0;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return err_response(StatusCode::BAD_REQUEST, &format!("multipart error: {e}")),
        };

        if field.name().unwrap_or("") != field_name {
            continue;
        }
        if cfg.max_files.is_some_and(|max| uploaded.len() >= max) {
            return err_response(StatusCode::BAD_REQUEST, "too many files");
        }

        let original_name = field.file_name().unwrap_or("upload").to_owned();
        let content_type_str = field.content_type().map(str::to_owned);

        if let Err(resp) = check_mime_type(content_type_str.as_deref(), cfg.allowed_mime_types.as_ref()) {
            return resp;
        }

        let data = match field.bytes().await {
            Ok(b) => b,
            Err(e) => return err_response(StatusCode::BAD_REQUEST, &format!("read error: {e}")),
        };

        let file_bytes = data.len() as u64;
        if cfg.max_file_size_bytes.is_some_and(|max| file_bytes > max) {
            return err_response(StatusCode::PAYLOAD_TOO_LARGE, "file exceeds maxFileSizeBytes");
        }
        total_bytes += file_bytes;
        if cfg.max_total_size_bytes.is_some_and(|max| total_bytes > max) {
            return err_response(StatusCode::PAYLOAD_TOO_LARGE, "upload exceeds maxTotalSizeBytes");
        }

        let mime = content_type_str.unwrap_or_else(|| {
            mime_guess::from_path(&original_name)
                .first_or_octet_stream()
                .to_string()
        });

        let save_name = match save_upload_file(&cfg.dir, &original_name, &data).await {
            Ok(n) => n,
            Err(resp) => return resp,
        };

        uploaded.push(json!({
            "name":         save_name,
            "originalName": original_name,
            "size":         file_bytes,
            "mimeType":     mime,
        }));
    }

    if uploaded.is_empty() {
        return err_response(StatusCode::BAD_REQUEST, "no files uploaded");
    }

    Json(json!({ "status": "ok", "files": uploaded })).into_response()
}

// ── upload_handler helpers ────────────────────────────────────────────────────

/// Build a JSON error response with the given status and message.
fn err_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

/// Validate the uploaded field's Content-Type against the configured allowlist.
///
/// Returns `Err(Response)` with HTTP 415 when the type is absent or not in the
/// list; returns `Ok(())` when no allowlist is configured or the type matches.
fn check_mime_type(
    content_type: Option<&str>,
    allowed_mime_types: Option<&Vec<String>>,
) -> Result<(), Response> {
    let Some(allowed) = allowed_mime_types else {
        return Ok(());
    };
    let ct = content_type.unwrap_or("");
    let ok = !ct.is_empty() && allowed.iter().any(|a| ct.starts_with(a.as_str()));
    if ok {
        return Ok(());
    }
    // Missing Content-Type is treated as a rejection — an unknown type cannot
    // be verified against the allowlist, so we refuse rather than bypass.
    let msg = if ct.is_empty() {
        "missing Content-Type; upload rejected by mime-type allowlist".to_owned()
    } else {
        format!("mime type not allowed: {ct}")
    };
    Err(err_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, &msg))
}

/// Write `data` to a new UUID-named file under `dir` and return the file name.
async fn save_upload_file(dir: &str, original_name: &str, data: &[u8]) -> Result<String, Response> {
    let ext = Path::new(original_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let save_name = format!("{}{ext}", uuid::Uuid::new_v4());
    let save_path = Path::new(dir).join(&save_name);
    tokio::fs::write(&save_path, data).await.map_err(|e| {
        err_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("write error: {e}"),
        )
    })?;
    Ok(save_name)
}
