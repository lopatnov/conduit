use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State};
use axum::http::{header::CONTENT_LENGTH, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use futures::StreamExt;
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
        //
        // Issue #286: axum 0.8's `{*path}` wildcard capture does not match
        // the empty segment at the root path "/" (confirmed against axum's
        // own routing source) — a client posting directly to the upload
        // service's root got a 404 instead of reaching `upload_handler`.
        // The explicit `/` route below covers that case; `{*path}` still
        // covers every non-root path as before.
        .route("/", post(upload_handler::<S>))
        .route("/{*path}", post(upload_handler::<S>))
        // No router-wide body limit: axum's own default (2 MiB) would cap every upload, and a fixed ceiling here
        // (it used to be 5 GiB, issues #277 and #700) is decided before the site, and so its configured limits,
        // is known: it capped a site that asked for more. The bound is applied per request from the site's config
        // (`request_body_ceiling`): `upload_handler` counts every byte of the body, whatever the field and
        // whether or not there is a `Content-Length` (chunked), and the handler refuses early on `Content-Length`
        // and chunk by chunk (`check_chunk_limits`, issue #277), so a site with no limits at all still cannot
        // fill the disk or hold a connection with an endless body.
        .layer(DefaultBodyLimit::disable())
        .with_state(state)
}

/// The request body ceiling of a site that sets no `maxTotalSizeBytes` (issue #277): 5 GiB, bounding the disk a site
/// with no limits configured at all can be made to fill. A site that sets `maxFileSizeBytes` above it gets that
/// instead (a configured single file always fits), and a site that sets `maxTotalSizeBytes` gets exactly that.
/// In `u64`: a `usize` cannot hold 5 GiB on a 32-bit target (issues #696 and #700).
const DEFAULT_MAX_REQUEST_BODY_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Room above the ceiling for the multipart framing (boundaries, part headers), which is part of the request body
/// but not of the file bytes the ceiling counts: 1 MiB, plus 0.1% of the ceiling (see `request_body_bound`).
const CONTENT_LENGTH_SLACK: u64 = 1024 * 1024;

/// The most file bytes one request to this site may carry (issue #700).
fn request_body_ceiling(cfg: &UploadConfig) -> u64 {
    cfg.max_total_size_bytes
        .unwrap_or_else(|| DEFAULT_MAX_REQUEST_BODY_BYTES.max(cfg.max_file_size_bytes.unwrap_or(0)))
}

/// The most bytes the whole request body may have: the ceiling plus the room for the multipart framing. The room is
/// 1 MiB and 0.1% of the ceiling, so that a request with a great many small files, whose framing grows with their
/// number, is not refused for it when it is at the ceiling (about 200 bytes of framing per file).
fn request_body_bound(ceiling: u64) -> u64 {
    ceiling
        .saturating_add(CONTENT_LENGTH_SLACK)
        .saturating_add(ceiling / 1024)
}

/// Whether a `Content-Length` header already shows the request body to be over its bound.
fn content_length_over_ceiling(headers: &HeaderMap, ceiling: u64) -> bool {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .is_some_and(|len| len > request_body_bound(ceiling))
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

/// The site this request is for, from the header the proxy injects (0 when absent or not a number).
fn site_index(headers: &HeaderMap) -> usize {
    headers
        .get(SITE_IDX_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Bounds the whole request body (issue #700): the body is replaced by a stream that counts every byte that is read,
/// in `u64`, and fails when the count passes `limit` (the site's ceiling plus the room for the multipart framing),
/// setting `over`. The handler's own checks only count the bytes of the field it saves, so without this a field with
/// another name, or a body with no `Content-Length` (chunked), could be pushed through without any bound. The error
/// multer reports for that is turned into a `413` by `bound_response`.
fn bound_request_body(req: Request, limit: u64, over: Arc<AtomicBool>) -> Request {
    let (parts, body) = req.into_parts();
    let mut seen: u64 = 0;
    let counted = body.into_data_stream().map(move |chunk| {
        let chunk = chunk?;
        seen = seen.saturating_add(chunk.len() as u64);
        if seen > limit {
            over.store(true, Ordering::Relaxed);
            return Err(axum::Error::new("request body is over its bound"));
        }
        Ok(chunk)
    });
    Request::from_parts(parts, Body::from_stream(counted))
}

/// The upload endpoint. The site's config is looked up once, here: a hot reload between two lookups could otherwise
/// give the byte counter and the rest of the handler different limits. It comes from the server's own state, never
/// from the request.
async fn upload_handler<S: UploadConfigSource>(
    State(state): State<Arc<S>>,
    req: Request,
) -> Response {
    let cfg = state.upload_config(site_index(req.headers()));
    let over = Arc::new(AtomicBool::new(false));
    let req = match &cfg {
        Some(cfg) => bound_request_body(
            req,
            request_body_bound(request_body_ceiling(cfg)),
            over.clone(),
        ),
        None => req,
    };
    let headers = req.headers().clone();
    let multipart = match Multipart::from_request(req, &state).await {
        Ok(m) => m,
        Err(rejection) => return rejection.into_response(),
    };
    // The flag is read after the request has been handled: it is set while the body is read.
    let response = handle_upload(cfg, headers, multipart).await;
    bound_response(over.load(Ordering::Relaxed), response)
}

/// A body that went over its bound surfaces from multer as a read error (a 400): it is a 413. A response that
/// succeeded, or a request whose body never went over, is left as it is.
fn bound_response(body_went_over: bool, response: Response) -> Response {
    if body_went_over && !response.status().is_success() {
        return err_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds maxTotalSizeBytes, or the default request ceiling when it is not set",
        );
    }
    response
}

/// The files a request has saved so far. An error status means nothing was kept, the same as for the file that was
/// being written when the error came (`process_upload_field`), so they are removed unless `keep` was called.
///
/// Removing a completed file can be slow (a multi-GiB file on a network or slow disk, issue #702), so it never runs
/// on an async worker: the error path awaits `discard`, which removes them on the blocking pool while only this
/// request waits; a request that is cancelled instead (the client goes away, the task is dropped) cannot await, so
/// its `Drop` hands the paths to the blocking pool and does not wait for them.
#[derive(Default)]
struct SavedFiles {
    paths: Vec<PathBuf>,
    keep: bool,
}

impl SavedFiles {
    fn push(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    /// The request succeeded: the files stay.
    fn keep(mut self) {
        self.keep = true;
    }

    /// The request failed: remove the files, off the async workers, and wait until they are gone, so that the
    /// error response is sent only after the cleanup.
    async fn discard(&mut self) {
        let paths = std::mem::take(&mut self.paths);
        if paths.is_empty() {
            return;
        }
        // A join error means the blocking task was cancelled, which only happens when the runtime shuts down.
        let _ = tokio::task::spawn_blocking(move || remove_files(&paths)).await;
    }
}

fn remove_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

impl Drop for SavedFiles {
    /// Only a cancelled request, or a panic, gets here with files left: the paths go to the blocking pool without
    /// waiting. Without a runtime (a plain unit test) they are removed on the spot. A runtime that is shutting down
    /// may drop the task, which is no different from the process being killed in the middle of a write.
    fn drop(&mut self) {
        if self.keep || self.paths.is_empty() {
            return;
        }
        let paths = std::mem::take(&mut self.paths);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                drop(handle.spawn_blocking(move || remove_files(&paths)));
            }
            Err(_) => remove_files(&paths),
        }
    }
}

#[allow(clippy::result_large_err)]
async fn handle_upload(
    cfg: Option<UploadConfig>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let cfg = match cfg {
        Some(c) => c,
        None => return err_response(StatusCode::NOT_FOUND, "no upload config for this site"),
    };

    if content_length_over_ceiling(&headers, request_body_ceiling(&cfg)) {
        return err_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds maxTotalSizeBytes, or the default request ceiling when it is not set",
        );
    }

    if let Err(e) = tokio::fs::create_dir_all(&cfg.dir).await {
        return err_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("could not create upload dir: {e}"),
        );
    }

    let field_name = cfg.field_name.as_deref().unwrap_or("file");
    let mut uploaded: Vec<serde_json::Value> = Vec::new();
    let mut saved = SavedFiles::default();
    let mut total_bytes: u64 = 0;

    let outcome: Result<(), Response> = async {
        loop {
            let field = match multipart.next_field().await {
                Ok(Some(f)) => f,
                Ok(None) => return Ok(()),
                Err(e) => {
                    return Err(err_response(
                        StatusCode::BAD_REQUEST,
                        &format!("multipart error: {e}"),
                    ))
                }
            };

            if field.name().unwrap_or("") != field_name {
                continue;
            }
            if cfg.max_files.is_some_and(|max| uploaded.len() >= max) {
                return Err(err_response(StatusCode::BAD_REQUEST, "too many files"));
            }

            let entry = process_upload_field(field, &cfg, &mut total_bytes, &mut saved).await?;
            uploaded.push(entry);
        }
    }
    .await;
    if let Err(response) = outcome {
        saved.discard().await;
        return response;
    }

    if uploaded.is_empty() {
        return err_response(StatusCode::BAD_REQUEST, "no files uploaded");
    }

    saved.keep();
    Json(json!({ "status": "ok", "files": uploaded })).into_response()
}

/// Process a single multipart field: validate type, stream to disk while
/// enforcing size limits per chunk, and return the resulting JSON entry.
///
/// Returns `Ok(json_entry)` on success or `Err(error_response)` on rejection; the file's path goes into `saved`.
// `Response` as the Err type is the idiomatic Axum short-circuit-to-HTTP-error
// pattern used throughout this file (via `err_response()` and `?`) -- boxing
// it would only add an allocation on the error path without changing behavior.
#[allow(clippy::result_large_err)]
async fn process_upload_field(
    field: axum::extract::multipart::Field<'_>,
    cfg: &UploadConfig,
    total_bytes: &mut u64,
    saved: &mut SavedFiles,
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
    // body first. The path is registered with `saved` BEFORE the file is
    // created, so that a rejection, an I/O error, or the request being
    // cancelled in the middle of the write (the client goes away) removes the
    // partly written file with the rest: a refused upload never leaves debris.
    saved.push(save_path.clone());
    let file_bytes = stream_field_to_file(field, &save_path, cfg, total_bytes).await?;

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
            "upload exceeds maxTotalSizeBytes, or the default request ceiling when it is not set",
        ));
    }
    Ok(())
}

/// `check_chunk_limits` with this site's limits: its own file limit, and as the request limit its ceiling
/// (`maxTotalSizeBytes`, or the default one when that is not set, issue #700).
#[allow(clippy::result_large_err)]
fn check_request_limits(
    chunk_len: u64,
    file_bytes_so_far: u64,
    total_bytes_so_far: u64,
    cfg: &UploadConfig,
) -> Result<(), Response> {
    check_chunk_limits(
        chunk_len,
        file_bytes_so_far,
        total_bytes_so_far,
        cfg.max_file_size_bytes,
        Some(request_body_ceiling(cfg)),
    )
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
        check_request_limits(
            chunk.len() as u64,
            file_bytes,
            *total_bytes + file_bytes,
            cfg,
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

    // tokio::fs::File offloads writes to a blocking-pool task; write_all
    // returning does not guarantee the last write actually landed. Flush
    // and await it before reporting success, or the final chunk can be
    // lost if the process shuts down before that background task runs.
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|e| {
            err_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("flush error: {e}"),
            )
        })?;

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

/// The longest extension of an uploaded file name that is kept.
const MAX_EXTENSION_LEN: usize = 16;

/// Compute a random UUID-based destination filename and its full path under `dir`. The original extension is kept
/// verbatim when it is ASCII letters and digits (at most `MAX_EXTENSION_LEN`); anything else is dropped, so that a
/// client's name cannot put a `:` (an NTFS alternate data stream on Windows), a space, a control or other special
/// character into the name of a file on the server.
fn destination_path(dir: &str, original_name: &str) -> (String, std::path::PathBuf) {
    let ext = Path::new(original_name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|e| {
            !e.is_empty()
                && e.len() <= MAX_EXTENSION_LEN
                && e.bytes().all(|b| b.is_ascii_alphanumeric())
        })
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
    fn destination_path_drops_an_extension_that_is_not_plain_letters_and_digits() {
        // `a:b` would make an NTFS alternate data stream on Windows; the others are special characters
        for name in [
            "x.a:b",
            "x.a b",
            "x.\u{e9}",
            "x.t\u{0}t",
            "x.a*b",
            "x.",
            "x.<b>",
            &format!("x.{}", "a".repeat(17)),
        ] {
            let (save_name, _) = destination_path("/uploads", name);
            assert!(
                !save_name.contains('.') && !save_name.contains(':'),
                "{name:?} must not leave an extension: {save_name}"
            );
        }
        // plain ones stay, up to the length limit
        assert!(destination_path("/u", "photo.jpeg").0.ends_with(".jpeg"));
        assert!(destination_path("/u", "a.b2").0.ends_with(".b2"));
        assert!(destination_path("/u", &format!("x.{}", "a".repeat(16)))
            .0
            .ends_with(&"a".repeat(16)));
    }

    #[test]
    fn saved_files_are_removed_when_dropped_unless_the_request_kept_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let make = |name: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            path
        };
        // dropped (an error, or a cancelled request): gone
        let mut dropped = SavedFiles::default();
        dropped.push(make("a"));
        dropped.push(make("b"));
        drop(dropped);
        assert!(!dir.path().join("a").exists() && !dir.path().join("b").exists());
        // kept (the request succeeded): stay
        let mut kept = SavedFiles::default();
        kept.push(make("c"));
        kept.keep();
        assert!(dir.path().join("c").exists());
        // a path that is already gone is no problem
        let mut gone = SavedFiles::default();
        gone.push(dir.path().join("never-existed"));
        drop(gone);
    }

    #[tokio::test]
    async fn discard_removes_the_files_before_it_returns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut saved = SavedFiles::default();
        for name in ["a", "b"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            saved.push(path);
        }
        saved.discard().await;
        // gone at once, with no waiting: the error response is sent only after this
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // and the guard has nothing left to do when it is dropped
        assert!(saved.paths.is_empty());
    }

    #[tokio::test]
    async fn a_guard_dropped_inside_a_runtime_removes_its_files_on_the_blocking_pool() {
        // The cancelled-request case: nothing can be awaited in a `Drop`, so the removal is not immediate.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut saved = SavedFiles::default();
        let path = dir.path().join("a");
        std::fs::write(&path, b"x").unwrap();
        saved.push(path.clone());
        drop(saved);
        let mut waited = 0;
        while path.exists() && waited < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            waited += 1;
        }
        assert!(
            !path.exists(),
            "the file must be removed by the blocking pool"
        );
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

    // ── make_upload_router root-path routing (issue #286) ──────────────────

    struct FixedConfig(UploadConfig);

    impl UploadConfigSource for FixedConfig {
        fn upload_config(&self, _site_idx: usize) -> Option<UploadConfig> {
            Some(self.0.clone())
        }
    }

    /// Sends a minimal single-file multipart POST to `path` on a freshly
    /// spawned real upload server and returns the response's status line.
    ///
    /// Drives the router end-to-end (`TcpListener` + `axum::serve`, a real
    /// TCP client) rather than mocking anything away, so it actually
    /// exercises axum's own route matching -- the exact layer issue #286's
    /// bug lived in (`{*path}` silently not matching the empty root segment).
    async fn post_multipart_file(path: &str) -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        let addr = spawn_upload_server(site_config(&dir, None, None)).await;
        send_multipart(addr, path, b"hello world").await
    }

    fn site_config(
        dir: &tempfile::TempDir,
        max_file_size_bytes: Option<u64>,
        max_total_size_bytes: Option<u64>,
    ) -> UploadConfig {
        UploadConfig {
            path: "/upload".to_owned(),
            dir: dir.path().to_string_lossy().into_owned(),
            max_file_size_bytes,
            max_total_size_bytes,
            max_files: None,
            allowed_mime_types: None,
            field_name: None,
        }
    }

    async fn spawn_upload_server(cfg: UploadConfig) -> std::net::SocketAddr {
        let state = Arc::new(FixedConfig(cfg));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, make_upload_router(state)).await;
        });
        addr
    }

    /// A multipart body of `(field name, bytes)` parts, each with a file name.
    fn multipart_body(fields: &[(&str, &[u8])]) -> Vec<u8> {
        let boundary = "conduit-test-boundary";
        let mut body = Vec::new();
        for (name, data) in fields {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\n\
                     Content-Disposition: form-data; name=\"{name}\"; filename=\"test.bin\"\r\n\
                     Content-Type: application/octet-stream\r\n\
                     \r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(data);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        body
    }

    /// Posts a one-file multipart body to `path` on the server at `addr` and returns the whole response as text.
    async fn send_multipart_response(
        addr: std::net::SocketAddr,
        path: &str,
        file: &[u8],
    ) -> String {
        send_fields(addr, path, &[("file", file)]).await
    }

    /// Posts a multipart body of several fields (with a `Content-Length`) and returns the whole response as text.
    async fn send_fields(
        addr: std::net::SocketAddr,
        path: &str,
        fields: &[(&str, &[u8])],
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let boundary = "conduit-test-boundary";
        let body = multipart_body(fields);
        let head = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Content-Type: multipart/form-data; boundary={boundary}\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            body.len()
        );

        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let (mut reader, mut writer) = stream.split();
        // Deliberately no `shutdown()` of the write side here: half-closing it before the server has `accept()`-ed
        // the connection can drop the still-pending backlog entry outright on some platforms (confirmed empirically
        // on Windows) -- the `Content-Length` header already tells the server exactly how much body to expect, and
        // the server's own `Connection: close` closes its end after responding, which terminates the read below.
        //
        // The answer is read while the body is still being written: a server that refuses the request closes the
        // connection under the writer (a broken pipe on Linux, a reset on Windows), which then simply stops.
        let write = async {
            if writer.write_all(head.as_bytes()).await.is_ok() {
                let _ = writer.write_all(&body).await;
            }
        };
        let mut response = Vec::new();
        let read = async {
            let _ = reader.read_to_end(&mut response).await;
        };
        tokio::join!(write, read);
        String::from_utf8_lossy(&response).into_owned()
    }

    /// Whether `status` shows a refusal for being too large. On Windows the client may see a reset connection and no
    /// answer at all when the server refuses a body it has not finished reading (so an empty status counts there);
    /// on Unix the 413 is always seen, and it is required.
    fn is_413(status: &str) -> bool {
        status.starts_with("HTTP/1.1 413") || (cfg!(windows) && status.is_empty())
    }

    /// The first line of the response (the status line).
    async fn send_multipart(addr: std::net::SocketAddr, path: &str, file: &[u8]) -> String {
        send_multipart_response(addr, path, file)
            .await
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    // ── what a rejected request leaves behind, and one config per request (CodeRabbit on #699) ──────

    #[tokio::test]
    async fn a_request_refused_for_a_later_file_keeps_none_of_its_earlier_files() {
        // two files in one request, the second over `maxFileSizeBytes`: the 413 means nothing was kept, so the
        // first file, which had completed, is removed as well as the half-written second one.
        let dir = tempfile::tempdir().expect("tempdir");
        let addr = spawn_upload_server(site_config(&dir, Some(100 * 1024), None)).await;
        let small = vec![1u8; 10 * 1024];
        let big = vec![2u8; 200 * 1024];
        let response = send_fields(addr, "/", &[("file", &small), ("file", &big)]).await;
        let status = response.lines().next().unwrap_or_default();
        assert!(is_413(status), "got: {status}");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "a rejected request must leave no file"
        );
    }

    #[tokio::test]
    async fn a_request_whose_body_goes_over_its_bound_keeps_none_of_its_files() {
        // A completed file, then 3 MiB in a field that is ignored, sent chunked: there is no Content-Length for the
        // early refusal, so only the byte counter can stop it (1 MiB total limit plus the framing room), and it
        // does so after the first file was saved. The request is refused and that file is removed again.
        let root = tempfile::tempdir().expect("tempdir");
        // a directory that does not exist yet: the handler creates it only for a request it reads, so that it
        // exists afterwards shows that this request got as far as saving the first file
        let uploads = root.path().join("uploads");
        let mut cfg = site_config(&root, None, Some(1024 * 1024));
        cfg.dir = uploads.to_string_lossy().into_owned();
        let addr = spawn_upload_server(cfg).await;
        let small = vec![1u8; 10 * 1024];
        let ignored = vec![3u8; 3 * 1024 * 1024];
        let status = send_chunked_fields(addr, &[("file", &small), ("other", &ignored)]).await;
        assert!(is_413(&status), "got: {status}");
        assert!(
            uploads.exists(),
            "the request was refused before it was read, so the cleanup was not exercised"
        );
        assert_eq!(std::fs::read_dir(&uploads).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_client_that_goes_away_in_the_middle_of_a_file_leaves_no_file() {
        // The request is abandoned while the file is being written. What the server then does (a body read error,
        // or the request being dropped) must remove the half-written file: its path is registered before the
        // write starts.
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let addr = spawn_upload_server(site_config(&dir, None, None)).await;
        let big = vec![9u8; 4 * 1024 * 1024];
        let body = multipart_body(&[("file", &big)]);
        let head = format!(
            "POST / HTTP/1.1\r\nHost: {addr}\r\n\
             Content-Type: multipart/form-data; boundary=conduit-test-boundary\r\n\
             Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        stream.write_all(head.as_bytes()).await.expect("write head");
        // the first 256 KiB of the body: the field header and the start of the file, no end
        let part = &body[..256 * 1024];
        stream
            .write_all(format!("{:x}\r\n", part.len()).as_bytes())
            .await
            .expect("write chunk size");
        stream.write_all(part).await.expect("write part");
        stream.write_all(b"\r\n").await.expect("write chunk end");
        stream.flush().await.expect("flush");
        // the server has started the file: wait until it is there
        let count = || std::fs::read_dir(dir.path()).map_or(0, |d| d.count());
        let mut waited = 0;
        while count() == 0 && waited < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            waited += 1;
        }
        assert_eq!(count(), 1, "the server never started the file");
        // the client goes away
        drop(stream);
        let mut waited = 0;
        while count() != 0 && waited < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            waited += 1;
        }
        assert_eq!(
            count(),
            0,
            "the half-written file of an abandoned request must be removed"
        );
    }

    #[tokio::test]
    async fn a_successful_request_keeps_all_its_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let addr = spawn_upload_server(site_config(&dir, None, None)).await;
        let a = vec![1u8; 1024];
        let b = vec![2u8; 2048];
        let response = send_fields(addr, "/", &[("file", &a), ("file", &b)]).await;
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    /// A config source that counts how often it is asked: a request must ask once, so that the byte counter and the
    /// handler cannot see two different configs when a hot reload lands between two lookups.
    struct CountingConfig {
        cfg: UploadConfig,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl UploadConfigSource for CountingConfig {
        fn upload_config(&self, _site_idx: usize) -> Option<UploadConfig> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some(self.cfg.clone())
        }
    }

    #[tokio::test]
    async fn a_request_looks_up_the_site_config_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(CountingConfig {
            cfg: site_config(&dir, None, None),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let served = state.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, make_upload_router(served)).await;
        });
        let response = send_multipart_response(addr, "/", b"hello").await;
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    }

    // ── the request body ceiling (issue #700) ──────────────────────────────────

    fn config_with(max_file: Option<u64>, max_total: Option<u64>) -> UploadConfig {
        UploadConfig {
            path: "/upload".to_owned(),
            dir: "./uploads".to_owned(),
            max_file_size_bytes: max_file,
            max_total_size_bytes: max_total,
            max_files: None,
            allowed_mime_types: None,
            field_name: None,
        }
    }

    #[test]
    fn the_ceiling_is_five_gib_without_limits_and_follows_the_configured_ones() {
        const GIB: u64 = 1024 * 1024 * 1024;
        // nothing configured: the default, computed in u64 (5 GiB does not fit a 32-bit usize)
        assert_eq!(request_body_ceiling(&config_with(None, None)), 5 * GIB);
        // a file limit below the default changes nothing; one above it is honoured (AI models are tens of GB)
        assert_eq!(
            request_body_ceiling(&config_with(Some(1024), None)),
            5 * GIB
        );
        assert_eq!(
            request_body_ceiling(&config_with(Some(50 * GIB), None)),
            50 * GIB
        );
        // a total limit is the ceiling, below or above the default
        assert_eq!(request_body_ceiling(&config_with(None, Some(100))), 100);
        assert_eq!(
            request_body_ceiling(&config_with(Some(10), Some(50 * GIB))),
            50 * GIB
        );
    }

    #[test]
    fn the_streaming_check_refuses_at_the_ceiling_of_a_site_without_limits() {
        const GIB: u64 = 1024 * 1024 * 1024;
        // a site without limits: refused at the default ceiling, accepted one byte below it
        let cfg = config_with(None, None);
        assert!(check_request_limits(1, 0, 5 * GIB - 1, &cfg).is_ok());
        assert!(check_request_limits(1, 0, 5 * GIB, &cfg).is_err());
        // a configured big file is not stopped by the default: its own limit is the ceiling
        let big = config_with(Some(50 * GIB), None);
        assert!(check_request_limits(1, 6 * GIB, 6 * GIB, &big).is_ok());
        assert!(check_request_limits(1, 50 * GIB, 50 * GIB, &big).is_err());
        // a total limit below the default is what applies
        let small = config_with(None, Some(100));
        assert!(check_request_limits(1, 0, 99, &small).is_ok());
        assert!(check_request_limits(1, 0, 100, &small).is_err());
    }

    #[test]
    fn a_body_over_its_bound_turns_a_read_error_into_a_413() {
        let bad_request = || err_response(StatusCode::BAD_REQUEST, "multipart error: read error");
        // over the bound: the 400 that multer's read error produced becomes a 413
        assert_eq!(
            bound_response(true, bad_request()).status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        // a body that never went over keeps its own status, and a success is never rewritten
        assert_eq!(
            bound_response(false, bad_request()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            bound_response(true, Json(json!({})).into_response()).status(),
            StatusCode::OK
        );
    }

    #[test]
    fn the_room_for_the_framing_grows_with_the_ceiling() {
        const GIB: u64 = 1024 * 1024 * 1024;
        // a small ceiling: the 1 MiB is what counts
        assert_eq!(request_body_bound(0), CONTENT_LENGTH_SLACK);
        assert_eq!(request_body_bound(1024), 1024 + CONTENT_LENGTH_SLACK + 1);
        // a big one: 0.1% more, so that thousands of small files at the ceiling still fit
        assert_eq!(
            request_body_bound(5 * GIB),
            5 * GIB + CONTENT_LENGTH_SLACK + 5 * GIB / 1024
        );
        assert!(request_body_bound(5 * GIB) - 5 * GIB > 6 * 1024 * 1024 - 1);
        // no overflow near the top of u64
        assert_eq!(request_body_bound(u64::MAX), u64::MAX);
    }

    #[test]
    fn a_content_length_over_the_ceiling_is_seen_before_the_body_is_read() {
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(CONTENT_LENGTH, v.parse().unwrap());
            h
        };
        let ceiling = 1000;
        // within the ceiling plus the framing room: read and checked chunk by chunk
        assert!(!content_length_over_ceiling(&with("1000"), ceiling));
        assert!(!content_length_over_ceiling(
            &with(&(ceiling + CONTENT_LENGTH_SLACK).to_string()),
            ceiling
        ));
        // beyond it: refused without reading
        assert!(content_length_over_ceiling(
            &with(&(ceiling + CONTENT_LENGTH_SLACK + 1).to_string()),
            ceiling
        ));
        // no length, or one that is not a number (chunked, garbage): the streaming check decides
        assert!(!content_length_over_ceiling(&HeaderMap::new(), ceiling));
        assert!(!content_length_over_ceiling(&with("lots"), ceiling));
        // a ceiling near u64::MAX does not overflow
        assert!(!content_length_over_ceiling(&with("5"), u64::MAX));
    }

    #[tokio::test]
    async fn a_site_without_limits_accepts_a_file_over_axums_default_body_limit() {
        // axum's own default body limit is 2 MiB: if the router layer is not disabled, a 3 MiB file is refused
        // although the site's ceiling is 5 GiB.
        let dir = tempfile::tempdir().expect("tempdir");
        let addr = spawn_upload_server(site_config(&dir, None, None)).await;
        let status = send_multipart(addr, "/", &vec![7u8; 3 * 1024 * 1024]).await;
        assert!(status.starts_with("HTTP/1.1 200"), "got: {status}");
    }

    #[tokio::test]
    async fn a_file_limit_above_the_old_fixed_ceiling_is_configurable() {
        // the per-site number decides: a site that sets a file limit takes it, here with a 3 MiB file under it
        let dir = tempfile::tempdir().expect("tempdir");
        let addr = spawn_upload_server(site_config(&dir, Some(10 * 1024 * 1024), None)).await;
        let status = send_multipart(addr, "/", &vec![7u8; 3 * 1024 * 1024]).await;
        assert!(status.starts_with("HTTP/1.1 200"), "got: {status}");
    }

    #[tokio::test]
    async fn a_total_limit_refuses_with_413_both_while_streaming_and_on_content_length() {
        let root = tempfile::tempdir().expect("tempdir");
        // a directory that does not exist yet: the handler creates it only for a request it is going to read
        let uploads = root.path().join("uploads");
        let mut cfg = site_config(&root, None, Some(1024 * 1024));
        cfg.dir = uploads.to_string_lossy().into_owned();
        let addr = spawn_upload_server(cfg).await;
        // A server that refuses a body it has not finished reading closes the connection on unread data, which
        // the client may see as a reset (Windows especially) instead of the 413: both mean "refused", an empty
        // answer is only accepted for that reason, and what must hold either way is that nothing stays on disk.
        let refused = is_413;

        // 3 MiB: over the ceiling plus the slack, refused from the header before the body is read, so the
        // upload directory is not even created
        let early = send_multipart(addr, "/", &vec![7u8; 3 * 1024 * 1024]).await;
        assert!(refused(&early), "got: {early}");
        assert!(
            !uploads.exists(),
            "an early refusal must not touch the disk"
        );

        // 1.5 MiB: the length is within the slack, so the body is streamed and refused on the way, and the
        // partly written file is removed again
        let streamed = send_multipart(addr, "/", &vec![7u8; 1536 * 1024]).await;
        assert!(refused(&streamed), "got: {streamed}");
        assert!(
            uploads.exists(),
            "the streamed request was read, so its directory exists"
        );
        assert_eq!(std::fs::read_dir(&uploads).unwrap().count(), 0);
    }

    /// A one-field multipart body, sent with `Transfer-Encoding: chunked` (so with no `Content-Length`) in
    /// 64 KiB chunks; returns the status line of the answer (empty when the connection was reset).
    async fn send_chunked(addr: std::net::SocketAddr, field: &str, data: &[u8]) -> String {
        send_chunked_fields(addr, &[(field, data)]).await
    }

    /// The same for a body of several fields.
    async fn send_chunked_fields(addr: std::net::SocketAddr, fields: &[(&str, &[u8])]) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let boundary = "conduit-test-boundary";
        let body = multipart_body(fields);

        let head = format!(
            "POST / HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Content-Type: multipart/form-data; boundary={boundary}\r\n\
             Transfer-Encoding: chunked\r\n\
             Connection: close\r\n\
             \r\n"
        );
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let (mut reader, mut writer) = stream.split();
        let write = async {
            let mut ok = writer.write_all(head.as_bytes()).await.is_ok();
            for chunk in body.chunks(64 * 1024) {
                if !ok {
                    break;
                }
                ok = writer
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .is_ok()
                    && writer.write_all(chunk).await.is_ok()
                    && writer.write_all(b"\r\n").await.is_ok();
            }
            if ok {
                let _ = writer.write_all(b"0\r\n\r\n").await;
            }
        };
        let mut response = Vec::new();
        // The answer is read while the body is still being written: a server that refuses early may close the
        // connection under the writer, which then simply stops.
        let read = async {
            let _ = reader.read_to_end(&mut response).await;
        };
        tokio::join!(write, read);
        String::from_utf8_lossy(&response)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    #[tokio::test]
    async fn a_chunked_body_in_another_field_is_bounded_too() {
        // Without the byte counter of `bound_request_body`, a field the handler does not save is read and thrown
        // away without limit, and a chunked body has no `Content-Length` for the early refusal: 3 MiB in a field
        // named "other" would be read to its end and answered with a 400 ("no files uploaded"). The bound is the
        // total limit (1 MiB) plus 1 MiB of framing room, so it is refused: a 413, or a reset connection.
        let dir = tempfile::tempdir().expect("tempdir");
        let addr = spawn_upload_server(site_config(&dir, None, Some(1024 * 1024))).await;
        let status = send_chunked(addr, "other", &vec![7u8; 3 * 1024 * 1024]).await;
        assert!(is_413(&status), "got: {status}");
    }

    #[tokio::test]
    async fn a_file_of_exactly_the_total_limit_is_accepted_with_its_multipart_framing() {
        // The counter sees the whole body, boundaries and part headers included, so its bound has room for them:
        // a file of exactly `maxTotalSizeBytes` must not be refused for the few hundred bytes of framing around it.
        let dir = tempfile::tempdir().expect("tempdir");
        let limit = 100 * 1024;
        let addr = spawn_upload_server(site_config(&dir, None, Some(limit))).await;
        let with_length = send_multipart(addr, "/", &vec![7u8; limit as usize]).await;
        assert!(
            with_length.starts_with("HTTP/1.1 200"),
            "got: {with_length}"
        );
        let chunked = send_chunked(addr, "file", &vec![7u8; limit as usize]).await;
        assert!(chunked.starts_with("HTTP/1.1 200"), "got: {chunked}");
        // one byte more is over the limit
        let over = send_multipart(addr, "/", &vec![7u8; limit as usize + 1]).await;
        assert!(is_413(&over), "got: {over}");
    }

    #[tokio::test]
    async fn a_chunked_upload_within_its_bound_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let addr = spawn_upload_server(site_config(&dir, None, Some(1024 * 1024))).await;
        let status = send_chunked(addr, "file", &vec![7u8; 300 * 1024]).await;
        assert!(status.starts_with("HTTP/1.1 200"), "got: {status}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn upload_router_root_path_accepts_post_not_404() {
        // Issue #286: axum 0.8's `{*path}` wildcard capture does not match
        // the empty segment at "/" -- before the fix, POSTing directly to
        // the upload server's root returned 404 instead of reaching
        // upload_handler.
        let status_line = post_multipart_file("/").await;
        assert!(
            status_line.starts_with("HTTP/1.1 200"),
            "root path must route to upload_handler, got: {status_line}"
        );
    }

    #[tokio::test]
    async fn upload_router_nonroot_path_still_accepts_post() {
        // Same server, non-root path -- proves the fix didn't regress the
        // pre-existing `{*path}` wildcard route.
        let status_line = post_multipart_file("/some/nested/path").await;
        assert!(
            status_line.starts_with("HTTP/1.1 200"),
            "non-root path must still route to upload_handler, got: {status_line}"
        );
    }
}
