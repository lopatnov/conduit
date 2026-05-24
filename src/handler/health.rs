use bytes::Bytes;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::handler::response::write_response;

static HEALTH_BODY: &[u8] = b"{\"status\":\"ok\"}";

/// Write a health-check response.
///
/// When `upstreams` is non-empty the response body includes a map of upstream
/// URLs to their health state (`"healthy"` or `"down"`):
///
/// ```json
/// {
///   "status": "ok",
///   "upstreams": {
///     "http://b1:4000": "healthy",
///     "http://b2:4000": "down"
///   }
/// }
/// ```
///
/// When `upstreams` is empty the minimal `{"status":"ok"}` body is used.
pub async fn handle_health(
    session: &mut Session,
    upstreams: &[(&str, bool)],
    extra: &[(String, String)],
) -> Result<()> {
    let body = if upstreams.is_empty() {
        Bytes::from_static(HEALTH_BODY)
    } else {
        let map: serde_json::Map<String, serde_json::Value> = upstreams
            .iter()
            .map(|(url, healthy)| {
                let state = if *healthy { "healthy" } else { "down" };
                (url.to_string(), serde_json::Value::String(state.to_owned()))
            })
            .collect();
        let obj = serde_json::json!({
            "status": "ok",
            "upstreams": map,
        });
        Bytes::from(obj.to_string())
    };

    write_response(session, 200, "application/json", body, extra).await
}
