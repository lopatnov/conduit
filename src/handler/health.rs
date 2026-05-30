use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::handler::response::write_response;
use crate::handler::LocalHandlerImpl;

static HEALTH_BODY: &[u8] = b"{\"status\":\"ok\"}";

/// Build the JSON body for a health-check response.
///
/// Extracted for unit testability — `handle_health` is a thin wrapper that
/// calls this and writes the result to the Pingora session.
fn build_health_body(upstreams: &[(&str, bool)]) -> Bytes {
    if upstreams.is_empty() {
        return Bytes::from_static(HEALTH_BODY);
    }
    let map: serde_json::Map<String, serde_json::Value> = upstreams
        .iter()
        .map(|(url, healthy)| {
            let state = if *healthy { "healthy" } else { "down" };
            (url.to_string(), serde_json::Value::String(state.to_owned()))
        })
        .collect();
    let obj = serde_json::json!({ "status": "ok", "upstreams": map });
    Bytes::from(obj.to_string())
}

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
    write_response(
        session,
        200,
        "application/json",
        build_health_body(upstreams),
        extra,
    )
    .await
}

/// Handler struct for health-check responses.
pub struct HealthHandler {
    pub extra_headers: Vec<(String, String)>,
    /// Pre-computed `(url, is_healthy)` pairs when `includeUpstreams` is set.
    pub upstream_pairs: Vec<(String, bool)>,
}

#[async_trait]
impl LocalHandlerImpl for HealthHandler {
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        let pairs_ref: Vec<(&str, bool)> = self
            .upstream_pairs
            .iter()
            .map(|(u, h)| (u.as_str(), *h))
            .collect();
        handle_health(session, &pairs_ref, &self.extra_headers).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_upstreams_returns_minimal_body() {
        let body = build_health_body(&[]);
        assert_eq!(&*body, b"{\"status\":\"ok\"}");
    }

    #[test]
    fn upstreams_included_in_body() {
        let pairs = [("http://a:4000", true), ("http://b:4000", false)];
        let body = build_health_body(&pairs);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["upstreams"]["http://a:4000"], "healthy");
        assert_eq!(v["upstreams"]["http://b:4000"], "down");
    }

    #[test]
    fn all_healthy_upstreams() {
        let pairs = [("http://a:4000", true), ("http://b:4000", true)];
        let body = build_health_body(&pairs);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["upstreams"]["http://a:4000"], "healthy");
        assert_eq!(v["upstreams"]["http://b:4000"], "healthy");
    }
}
