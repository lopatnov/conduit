//! `POST` / `DELETE /ip-deny`: the dynamic deny list.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use conduit_runtime::proxy::service::AppState;

use super::error::{AdminError, AdminResult};

// ── Dynamic IP deny-list ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct IpDenyBody {
    /// CIDR to add or remove, e.g. `"1.2.3.0/24"` or `"10.0.0.5"`.
    pub(super) cidr: String,
}

/// Validate that a string is a valid IP address or CIDR notation. The same check as `ipFilter`, so a rule the matcher
/// would ignore (an IPv4-mapped network shorter than /96) is refused here instead of being accepted and denying nothing.
pub(super) fn validate_cidr(s: &str) -> bool {
    conduit_config_core::cidr::is_valid_ip_or_cidr(s)
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
