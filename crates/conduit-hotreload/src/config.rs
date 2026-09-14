use serde::{Deserialize, Serialize};

// ── Hot reload ─────────────────────────────────────────────────────────────

/// `false` | `true` | `{ "extensions": [".html", ".css"] }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum HotReloadConfig {
    Enabled(bool),
    Options(HotReloadOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HotReloadOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}
