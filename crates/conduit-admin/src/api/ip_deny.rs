//! `POST` / `DELETE /ip-deny`: the dynamic deny list.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::proxy::service::AppState;

use super::error::{AdminError, AdminResult};

// ── Dynamic IP deny-list ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct IpDenyBody {
    /// CIDR to add or remove, e.g. `"1.2.3.0/24"` or `"10.0.0.5"`.
    pub(super) cidr: String,
}

/// Validate that a string is a valid IP address or CIDR notation.
pub(super) fn validate_cidr(s: &str) -> bool {
    if let Some((addr_str, prefix_str)) = s.split_once('/') {
        if let Ok(prefix) = prefix_str.parse::<u32>() {
            if let Ok(addr) = addr_str.parse::<std::net::IpAddr>() {
                let max_prefix = if addr.is_ipv4() { 32 } else { 128 };
                return prefix <= max_prefix;
            }
        }
        return false;
    }
    s.parse::<std::net::IpAddr>().is_ok()
}

/// `POST /ip-deny` — add a CIDR to the runtime deny-list.
pub(super) async fn ip_deny_add_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IpDenyBody>,
) -> AdminResult<Json<Value>> {
    let cidr = body.cidr.trim().to_owned();
    if !validate_cidr(&cidr) {
        return Err(AdminError::BadRequest(format!(
            "invalid CIDR or IP address: {cidr:?}"
        )));
    }
    {
        let mut list = state
            .dynamic_deny
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !list.contains(&cidr) {
            list.push(cidr.clone());
        }
    }
    Ok(Json(
        json!({ "status": "ok", "action": "added", "cidr": cidr }),
    ))
}

/// `DELETE /ip-deny` — remove a CIDR from the runtime deny-list.
pub(super) async fn ip_deny_remove_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IpDenyBody>,
) -> Json<Value> {
    let cidr = body.cidr.trim().to_owned();
    {
        let mut list = state
            .dynamic_deny
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        list.retain(|c| c != &cidr);
    }
    Json(json!({ "status": "ok", "action": "removed", "cidr": cidr }))
}
