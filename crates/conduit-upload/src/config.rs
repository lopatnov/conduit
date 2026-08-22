use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UploadConfig {
    pub path: String,
    pub dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_file_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_mime_types: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_name: Option<String>,
}
