use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

// ── Static files ───────────────────────────────────────────────────────────

/// `"./dist"` | `["./a", "./b"]` | `{ "/": "./dist", "/docs": "./docs-dist" }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StaticConfig {
    Single(String),
    Multi(Vec<String>),
    Mapped(IndexMap<String, String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StaticOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<bool>,
    /// Duration string parsed with humantime: "1d", "30m", "1h"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_age: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<Vec<String>>,
    /// "ignore" | "allow" | "deny"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dot_files: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_compressed: Option<bool>,
}

// ── Fallback ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FallbackConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    /// Content-negotiated fallback rules keyed by Accept type ("html", "json", "*")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_accept: Option<IndexMap<String, FallbackRule>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FallbackRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
}
