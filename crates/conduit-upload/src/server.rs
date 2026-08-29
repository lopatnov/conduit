use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{DefaultBodyLimit, Multipart, State};
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
        // Hard backstop, independent of per-site `maxFileSizeBytes`/
        // `maxTotalSizeBytes` (issue #277): those are `Option<u64>` and
        // resolved per-request from site config the router doesn't have
        // at construction time, so a site with no limits configured at all
        // would otherwise have no upper bound whatsoever on request body
        // size (streaming to disk closes the *memory*-exhaustion vector,
        // but an unconfigured site could still fill the disk). This is a
        // blunt, generous global ceiling meant to catch that
        // no-limits-configured case, not to replace the precise per-site
        // streaming checks in `check_chunk_limits` — those still run first
        // and reject with the more specific 413 message.
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state)
}

/// Absolute ceiling on a single upload request's body size, regardless of
/// per-site config (see `make_upload_router`'s doc comment). 5 GiB —
/// generous enough not to interfere with legitimate large-file uploads
/// (video, datasets) while still bounding the worst case for a site that
/// hasn't configured `maxFileSizeBytes`/`maxTotalSizeBytes` at all.
const MAX_REQUEST_BODY_BYTES: usize = 5 * 1024 * 1024 * 1024;

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

/// Process a single multipart field: validate type, stream to disk while
/// enforcing size limits per chunk, and return the resulting JSON entry.
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

    let mime = content_type_str.unwrap_or_else(|| {
        mime_guess::from_path(&original_name)
            .first_or_octet_stream()
            .to_string()
    });

    let (save_name, save_path) = destination_path(&cfg.dir, &original_name);

    // Issue #277: stream the field to disk chunk by chunk instead of
    // buffering the whole body in memory via `field.bytes()` before any
    // size check ran — a client posting a part larger than the configured
    // limit no longer forces the server to allocate (and hold) the entire
    // body first. On any rejection or I/O error, remove the partially
    // written file so a rejected upload never leaves debris on disk.
    let file_bytes = match stream_field_to_file(field, &save_path, cfg, total_bytes).await {
        Ok(n) => n,
        Err(e) => {
            let _ = tokio::fs::remove_file(&save_path).await;
            return Err(e);
        }
    };

    Ok(json!({
        "name":         save_name,
        "originalName": original_name,
        "size":         file_bytes,
        "mimeType":     mime,
    }))
}

/// Whether writing `chunk_len` more bytes would push this field or the
/// whole request over its configured limit.
///
/// Pure and side-effect-free so the actual enforcement boundary — checked
/// as each chunk streams in, not once after the whole field has already
/// been buffered — is directly unit-testable (issue #277).
#[allow(clippy::result_large_err)]
fn check_chunk_limits(
    chunk_len: u64,
    file_bytes_so_far: u64,
    total_bytes_so_far: u64,
    max_file_size_bytes: Option<u64>,
    max_total_size_bytes: Option<u64>,
) -> Result<(), Response> {
    if max_file_size_bytes.is_some_and(|max| file_bytes_so_far + chunk_len > max) {
        return Err(err_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file exceeds maxFileSizeBytes",
        ));
    }
    if max_total_size_bytes.is_some_and(|max| total_bytes_so_far + chunk_len > max) {
        return Err(err_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "upload exceeds maxTotalSizeBytes",
        ));
    }
    Ok(())
}

/// Stream `field`'s body to `save_path`, checking `check_chunk_limits`
/// before writing each chunk. Returns the number of bytes written.
// See `process_upload_field`'s allow above -- same idiomatic pattern.
#[allow(clippy::result_large_err)]
async fn stream_field_to_file(
    mut field: axum::extract::multipart::Field<'_>,
    save_path: &Path,
    cfg: &UploadConfig,
    total_bytes: &mut u64,
) -> Result<u64, Response> {
    let mut file = tokio::fs::File::create(save_path).await.map_err(|e| {
        err_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("could not create destination file: {e}"),
        )
    })?;

    let mut file_bytes: u64 = 0;
    loop {
        let chunk = match field.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                return Err(err_response(
                    StatusCode::BAD_REQUEST,
                    &format!("read error: {e}"),
                ))
            }
        };
        // *total_bytes only accounts for *previously completed* fields --
        // it's updated once, after this loop, not per chunk. Without adding
        // file_bytes (this field's own running total), the request-wide
        // check would stay frozen at the pre-field value for the entire
        // duration of streaming this field, letting a single large field
        // bypass maxTotalSizeBytes almost entirely (Gitar finding on this
        // PR).
        check_chunk_limits(
            chunk.len() as u64,
            file_bytes,
            *total_bytes + file_bytes,
            cfg.max_file_size_bytes,
            cfg.max_total_size_bytes,
        )?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| {
                err_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("write error: {e}"),
                )
            })?;
        file_bytes += chunk.len() as u64;
    }

    *total_bytes += file_bytes;
    Ok(file_bytes)
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

/// Compute a random UUID-based destination filename (preserving the
/// original extension, verbatim) and its full path under `dir`.
fn destination_path(dir: &str, original_name: &str) -> (String, std::path::PathBuf) {
    let ext = Path::new(original_name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let save_name = format!("{}{ext}", uuid::Uuid::new_v4());
    let save_path = Path::new(dir).join(&save_name);
    (save_name, save_path)
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

    #[test]
    fn destination_path_preserves_extension_verbatim() {
        let (save_name, save_path) = destination_path("/uploads", "photo.JPG");
        assert!(
            save_name.ends_with(".JPG"),
            "extension from original_name must be preserved verbatim: {save_name}"
        );
        assert_eq!(save_path, std::path::Path::new("/uploads").join(&save_name));
    }

    #[test]
    fn destination_path_without_extension_has_no_dot_suffix() {
        let (save_name, _) = destination_path("/uploads", "noextension");
        assert!(
            !save_name.contains('.'),
            "a name with no extension must not gain a spurious '.': {save_name}"
        );
    }

    #[test]
    fn destination_path_is_randomized_per_call() {
        let (a, _) = destination_path("/uploads", "same.txt");
        let (b, _) = destination_path("/uploads", "same.txt");
        assert_ne!(
            a, b,
            "two uploads of the same original filename must not collide"
        );
    }

    // ── check_chunk_limits (issue #277: incremental, not buffer-then-check) ────

    #[test]
    fn check_chunk_limits_no_limits_configured_always_allows() {
        assert!(check_chunk_limits(1_000_000, 0, 0, None, None).is_ok());
    }

    #[test]
    fn check_chunk_limits_rejects_when_file_limit_exceeded_by_this_chunk() {
        // 5 bytes already written for this field + a 10-byte chunk = 15,
        // over a 10-byte max_file_size_bytes.
        let result = check_chunk_limits(10, 5, 5, Some(10), None);
        assert!(
            result.is_err(),
            "must reject as soon as this chunk would cross the file limit"
        );
    }

    #[test]
    fn check_chunk_limits_allows_chunk_landing_exactly_on_the_limit() {
        // 0 bytes so far + a 10-byte chunk = exactly 10, the configured max
        // -- must be allowed (over, not at-or-over).
        assert!(check_chunk_limits(10, 0, 0, Some(10), None).is_ok());
    }

    #[test]
    fn check_chunk_limits_rejects_when_total_limit_exceeded_even_if_file_limit_is_fine() {
        // This field's own running total (3) + chunk (5) = 8, well under a
        // generous 1000-byte per-file cap -- but the *request-wide* running
        // total (already at 95 from earlier fields) pushes the combined
        // total to 100, over a 99-byte maxTotalSizeBytes. Proves the two
        // limits are independent, not just the per-file one re-used twice.
        let result = check_chunk_limits(5, 3, 95, Some(1000), Some(99));
        assert!(
            result.is_err(),
            "the request-wide total limit must be enforced independently of the per-file limit"
        );
    }

    #[test]
    fn check_chunk_limits_first_chunk_over_limit_is_rejected_immediately() {
        // The exact property issue #277 is about: a single chunk that by
        // itself already exceeds the limit must be caught before it's ever
        // written, not after accumulating the whole field first.
        let result = check_chunk_limits(1_000_000, 0, 0, Some(1024), None);
        assert!(result.is_err());
    }
}
