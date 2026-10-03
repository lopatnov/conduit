//! `MiddlewareEntry` — the `sites[].middleware[]` config struct.
//!
//! Always compiled (see the crate root doc comment for why) — parses
//! regardless of whether `rhai`/`wasm` are enabled, so `feature_warnings()`
//! can keep warning on a config-without-feature mismatch instead of the
//! value silently vanishing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MiddlewareEntry {
    // `type` is a Rust keyword; r# prefix lets us use it as an identifier
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// File path to the script / WASM module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Pipeline phase to run this entry in.
    ///
    /// - `"request"` (default) — run during the request phase (before upstream)
    /// - `"response"` — run during the response phase (after upstream responds)
    ///
    /// WASM plugins do not need this field: if the module exports `on_response`,
    /// it is called automatically in both phases (request AND response).
    /// For Rhai scripts, set `phase: "response"` to run on the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}
