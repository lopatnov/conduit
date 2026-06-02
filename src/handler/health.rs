use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::handler::response::write_response;
use crate::handler::LocalHandlerImpl;

static HEALTH_BODY: &[u8] = b"{\"status\":\"ok\"}";

/// Per-upstream health information included in extended health responses.
#[derive(Debug, Clone)]
pub struct UpstreamHealthInfo {
    pub url: String,
    pub healthy: bool,
    pub latency_ms: Option<u64>,
    pub ejected: bool,
    pub consecutive_5xx: u32,
}

/// Build the JSON body for a health-check response.
///
/// When `upstreams` is non-empty, returns extended upstream health data:
/// ```json
/// {
///   "status": "ok",
///   "upstreams": [
///     { "url": "http://b1:4000", "healthy": true,  "latency_ms": 12, "ejected": false, "consecutive_5xx": 0 },
///     { "url": "http://b2:4000", "healthy": false, "latency_ms": null, "ejected": true, "consecutive_5xx": 5 }
///   ]
/// }
/// ```
fn build_health_body(upstreams: &[UpstreamHealthInfo]) -> Bytes {
    if upstreams.is_empty() {
        return Bytes::from_static(HEALTH_BODY);
    }
    let list: Vec<serde_json::Value> = upstreams
        .iter()
        .map(|u| {
            serde_json::json!({
                "url":             u.url,
                "healthy":         u.healthy,
                "latency_ms":      u.latency_ms,
                "ejected":         u.ejected,
                "consecutive_5xx": u.consecutive_5xx,
            })
        })
        .collect();
    let obj = serde_json::json!({ "status": "ok", "upstreams": list });
    Bytes::from(obj.to_string())
}

/// Write a health-check response.
///
/// When `upstreams` is non-empty the response body includes detailed per-upstream
/// health data (latency, ejection status, consecutive 5xx count).
/// When `upstreams` is empty the minimal `{"status":"ok"}` body is used.
pub async fn handle_health(
    session: &mut Session,
    upstreams: &[UpstreamHealthInfo],
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
    /// Extended per-upstream health info when `includeUpstreams` is set.
    pub upstream_infos: Vec<UpstreamHealthInfo>,
}

#[async_trait]
impl LocalHandlerImpl for HealthHandler {
    async fn handle(&mut self, session: &mut Session) -> Result<()> {
        handle_health(session, &self.upstream_infos, &self.extra_headers).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(
        url: &str,
        healthy: bool,
        latency_ms: Option<u64>,
        ejected: bool,
        consec_5xx: u32,
    ) -> UpstreamHealthInfo {
        UpstreamHealthInfo {
            url: url.to_owned(),
            healthy,
            latency_ms,
            ejected,
            consecutive_5xx: consec_5xx,
        }
    }

    #[test]
    fn empty_upstreams_returns_minimal_body() {
        let body = build_health_body(&[]);
        assert_eq!(&*body, b"{\"status\":\"ok\"}");
    }

    #[test]
    fn upstreams_included_in_body() {
        let infos = [
            info("http://a:4000", true, Some(12), false, 0),
            info("http://b:4000", false, None, true, 5),
        ];
        let body = build_health_body(&infos);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], "ok");
        let ups = v["upstreams"].as_array().unwrap();
        let a = ups.iter().find(|u| u["url"] == "http://a:4000").unwrap();
        let b = ups.iter().find(|u| u["url"] == "http://b:4000").unwrap();
        assert_eq!(a["healthy"], true);
        assert_eq!(a["latency_ms"], 12);
        assert_eq!(a["ejected"], false);
        assert_eq!(a["consecutive_5xx"], 0);
        assert_eq!(b["healthy"], false);
        assert_eq!(b["ejected"], true);
        assert_eq!(b["consecutive_5xx"], 5);
    }

    #[test]
    fn all_healthy_upstreams() {
        let infos = [
            info("http://a:4000", true, Some(10), false, 0),
            info("http://b:4000", true, Some(15), false, 0),
        ];
        let body = build_health_body(&infos);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let ups = v["upstreams"].as_array().unwrap();
        assert!(ups.iter().all(|u| u["healthy"] == true));
    }
}
