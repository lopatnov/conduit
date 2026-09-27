//! `GET /rate-limits`.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;

use conduit_runtime::proxy::service::AppState;

/// `GET /rate-limits` — per-site/route rate-limiter counters.
///
/// Returns a nested object: `{ site: { route_key: { passed, rejected } } }`.
/// `route_key` is `"*"` for the site-level (non-route) bucket.
///
/// Bucket keys are per-*client* (`rate_limit::site_key`/`route_key` include
/// the client key), so this aggregates (sums) every client-keyed bucket
/// that shares a `(site, route)` pair, rather than assuming a 1:1 mapping —
/// there is no other way to report meaningful site/route-level totals, and
/// summing avoids leaking individual client keys/IPs in the response.
/// Consumer-level buckets (`consumer\0{username}`) are deliberately excluded
/// — they're global, not attributable to one site or route.
pub(super) async fn rate_limits_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    use serde_json::Map;

    #[derive(Default)]
    struct Agg {
        passed: u64,
        rejected: u64,
    }

    let mut agg: std::collections::BTreeMap<(String, String), Agg> =
        std::collections::BTreeMap::new();

    for entry in state.rate_limiter.iter() {
        let parts: Vec<&str> = entry.key().split('\0').collect();
        let (site, route) = match parts.as_slice() {
            ["site", site_label, _client] => ((*site_label).to_owned(), "*".to_owned()),
            ["route", site_label, route_key, _client] => {
                ((*site_label).to_owned(), (*route_key).to_owned())
            }
            _ => continue, // "consumer\0{username}" or an unrecognized shape — not a site/route bucket
        };
        let bucket = entry.value();
        let a = agg.entry((site, route)).or_default();
        a.passed += bucket.passed;
        a.rejected += bucket.rejected;
    }

    let mut result: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    for ((site, route), a) in agg {
        result
            .entry(site)
            .or_insert_with(|| serde_json::Value::Object(Map::new()))
            .as_object_mut()
            .unwrap()
            .insert(route, json!({ "passed": a.passed, "rejected": a.rejected }));
    }

    Json(json!(result))
}
