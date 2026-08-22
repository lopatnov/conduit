use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Multipart, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;
use serde_json::json;
use tokio::net::TcpListener;

use crate::config::UploadConfig;

/// Header injected by the proxy to tell the upload server which site's config to use.
pub const SITE_IDX_HEADER: &str = "x-conduit-site-idx";

/// Supplies the active [`UploadConfig`] for a given site index at request time.
///
/// Lets this crate serve uploads without depending on the root crate's
/// `AppConfig`/`SiteConfig` types — see `CONTRIBUTING.md`'s crate-extraction
/// recipe, "Generic-in-crate, bound-by-type-alias-in-root".
pub trait UploadConfigSource: Send + Sync + 'static {
    /// Look up the current `upload` config for the site at `site_idx`, if any.
    fn upload_config(&self, site_idx: usize) -> Option<UploadConfig>;
}

/// Pingora `BackgroundService` that drives the Axum file-upload server.
///
/// The underlying `std::net::TcpListener` is pre-bound by the caller so that
/// the OS-assigned port is known before Pingora starts accepting proxy
/// traffic.  The listener is converted to a Tokio listener inside `start()`,
/// which runs on Pingora's async runtime.
pub struct UploadService<S: UploadConfigSource> {
    pub state: Arc<S>,
    /// Pre-bound standard listener.  Wrapped in a `Mutex<Option<…>>` so it
    /// can be taken exactly once when `start()` is called.
    listener: tokio::sync::Mutex<Option<std::net::TcpListener>>,
}

impl<S: UploadConfigSource> UploadService<S> {
    pub fn new(state: Arc<S>, listener: std::net::TcpListener) -> Self {
        Self {
            state,
            listener: tokio::sync::Mutex::new(Some(listener)),
        }
    }
}

#[async_trait]
impl<S: UploadConfigSource> BackgroundService for UploadService<S> {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        let std_listener = self
            .listener
            .lock()
            .await
            .take()
            .expect("UploadService::start called twice");

        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("upload server: failed to convert listener: {e}");
                return;
            }
        };

        if let Ok(addr) = listener.local_addr() {
            tracing::info!("upload server listening on http://{addr}");
        }

        if let Err(e) = axum::serve(listener, make_upload_router(self.state.clone()))
            .with_graceful_shutdown(async move {
                shutdown.changed().await.ok();
            })
            .await
        {
            tracing::error!("upload server exited with error: {e}");
        }
    }
}

/// Build the Axum `Router` for the upload server.
///
/// Exposed so that [`UploadService`] can serve it with a graceful-shutdown
/// handle, while tests can drive it directly.
pub fn make_upload_router<S: UploadConfigSource>(state: Arc<S>) -> Router {
    Router::new()
        // Accept POST to any path — the actual upload.path matching is done
        // by the Pingora router before the request reaches this server.
        .route("/{*path}", post(upload_handler::<S>))
        .with_state(state)
}

/// Start the Axum file-upload server on the given `listener` and serve
/// requests until the process exits.
///
/// `state` is shared with the Pingora proxy so that the server can read the
/// current `upload` configuration for a given site.
pub async fn run_upload_server<S: UploadConfigSource>(listener: TcpListener, state: Arc<S>) {
    if let Err(e) = axum::serve(listener, make_upload_router(state)).await {
        tracing::error!("upload server exited with error: {e}");
    }
}

async fn upload_handler<S: UploadConfigSource>(
    State(state): State<Arc<S>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let site_idx: usize = headers
        .get(SITE_IDX_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let cfg = match state.upload_config(site_idx) {
        Some(c) => c,
        None => return err_response(StatusCode::NOT_FOUND, "no upload config for this site"),
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
            Err(e) => {
                return err_response(StatusCode::BAD_REQUEST, &format!("multipart error: {e}"))
            }
        };

        if field.name().unwrap_or("") != field_name {
            continue;
        }
        if cfg.max_files.is_some_and(|max| uploaded.len() >= max) {
            return err_response(StatusCode::BAD_REQUEST, "too many files");
        }

        match process_upload_field(field, &cfg, &mut total_bytes).await {
            Ok(entry) => uploaded.push(entry),
            Err(resp) => return resp,
        }
    }

    if uploaded.is_empty() {
        return err_response(StatusCode::BAD_REQUEST, "no files uploaded");
    }

    Json(json!({ "status": "ok", "files": uploaded })).into_response()
}

/// Process a single multipart field: validate type, check sizes, write to disk.
///
/// Returns `Ok(json_entry)` on success or `Err(error_response)` on rejection.
// `Response` as the Err type is the idiomatic Axum short-circuit-to-HTTP-error
// pattern used throughout this file (via `err_response()` and `?`) -- boxing
// it would only add an allocation on the error path without changing behavior.
#[allow(clippy::result_large_err)]
async fn process_upload_field(
    field: axum::extract::multipart::Field<'_>,
    cfg: &UploadConfig,
    total_bytes: &mut u64,
) -> Result<serde_json::Value, Response> {
    let original_name = field.file_name().unwrap_or("upload").to_owned();
    let content_type_str = field.content_type().map(str::to_owned);

    check_mime_type(content_type_str.as_deref(), cfg.allowed_mime_types.as_ref())?;

    let data = field
        .bytes()
        .await
        .map_err(|e| err_response(StatusCode::BAD_REQUEST, &format!("read error: {e}")))?;

    let file_bytes = data.len() as u64;
    if cfg.max_file_size_bytes.is_some_and(|max| file_bytes > max) {
        return Err(err_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file exceeds maxFileSizeBytes",
        ));
    }
    *total_bytes += file_bytes;
    if cfg
        .max_total_size_bytes
        .is_some_and(|max| *total_bytes > max)
    {
        return Err(err_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "upload exceeds maxTotalSizeBytes",
        ));
    }

    let mime = content_type_str.unwrap_or_else(|| {
        mime_guess::from_path(&original_name)
            .first_or_octet_stream()
            .to_string()
    });

    let save_name = save_upload_file(&cfg.dir, &original_name, &data).await?;

    Ok(json!({
        "name":         save_name,
        "originalName": original_name,
        "size":         file_bytes,
        "mimeType":     mime,
    }))
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
///
/// `Response` is intentionally kept as the `Err` variant (rather than `Box<Response>`)
/// so the caller can directly `return resp` without an extra dereference.
#[allow(clippy::result_large_err)]
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
// See `process_upload_field`'s allow above -- same idiomatic pattern.
#[allow(clippy::result_large_err)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_allowlist_accepts_any_content_type_including_absent() {
        assert!(check_mime_type(Some("image/png"), None).is_ok());
        assert!(check_mime_type(None, None).is_ok());
    }

    #[test]
    fn allowlist_accepts_matching_prefix() {
        let allowed = vec!["image/".to_string(), "text/plain".to_string()];
        assert!(check_mime_type(Some("image/png"), Some(&allowed)).is_ok());
        assert!(check_mime_type(Some("text/plain"), Some(&allowed)).is_ok());
    }

    #[test]
    fn allowlist_rejects_non_matching_type_with_415() {
        let allowed = vec!["image/".to_string()];
        let err = check_mime_type(Some("application/pdf"), Some(&allowed)).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[test]
    fn missing_content_type_with_allowlist_is_rejected() {
        let allowed = vec!["image/".to_string()];
        let err = check_mime_type(None, Some(&allowed)).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// Regression test for the `!ct.is_empty()` guard specifically: without
    /// it, `allowed.iter().any(|a| ct.starts_with(a))` on an empty `ct`
    /// (missing Content-Type) matches an allowlist that happens to contain an
    /// empty-string entry — e.g. from a stray trailing comma in a YAML/JSON
    /// `allowedMimeTypes` list — since `"".starts_with("")` is `true`. That
    /// would silently accept an upload with no Content-Type instead of
    /// rejecting it. Verified empirically: removing the guard makes this
    /// test fail (content type wrongly accepted); restoring it makes it pass.
    #[test]
    fn missing_content_type_is_rejected_even_with_empty_string_allowlist_entry() {
        let allowed = vec!["".to_string()];
        let result = check_mime_type(None, Some(&allowed));
        assert!(
            result.is_err(),
            "missing Content-Type must never be accepted, even against a \
             degenerate allowlist containing an empty string"
        );
    }

    #[tokio::test]
    async fn save_upload_file_preserves_extension_and_writes_real_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir_path = dir.path().to_str().expect("utf8 path");
        let data = b"hello upload";

        let save_name = save_upload_file(dir_path, "photo.JPG", data)
            .await
            .expect("save should succeed");

        assert!(
            save_name.ends_with(".JPG"),
            "extension from original_name must be preserved verbatim: {save_name}"
        );
        let written = tokio::fs::read(dir.path().join(&save_name))
            .await
            .expect("saved file must exist with the exact returned name");
        assert_eq!(written, data, "saved file content must match input bytes");
    }

    #[tokio::test]
    async fn save_upload_file_without_extension_has_no_dot_suffix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir_path = dir.path().to_str().expect("utf8 path");

        let save_name = save_upload_file(dir_path, "noextension", b"data")
            .await
            .expect("save should succeed");

        assert!(
            !save_name.contains('.'),
            "a name with no extension must not gain a spurious '.': {save_name}"
        );
    }
}
