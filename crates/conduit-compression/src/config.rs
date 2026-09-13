use serde::{Deserialize, Serialize};

/// `false` | `true` | `{ "algorithms": ["br", "gzip"], "level": 6, "minBytes": 1024 }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum CompressionConfig {
    Enabled(bool),
    Options(CompressionOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CompressionOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithms: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_bytes: Option<u64>,
    /// Content-Type patterns to compress.
    ///
    /// **nginx `gzip_types` pattern.**  Only responses whose `Content-Type` matches
    /// one of these prefixes are compressed.  Compressing binary content (images,
    /// video, PDFs, already-compressed archives) wastes CPU and often increases
    /// the response size.
    ///
    /// Default: `["text/", "application/json", "application/xml",
    ///            "application/javascript", "application/xhtml", "image/svg"]`
    ///
    /// Set `["*"]` or `[""]` to compress all content types (not recommended).
    ///
    /// ```json
    /// { "algorithms": ["br", "gzip"],
    ///   "types": ["text/html", "text/css", "application/json"] }
    /// ```
    #[serde(rename = "types", skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
}
