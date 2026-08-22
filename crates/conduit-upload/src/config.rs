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

// serde_json is only pulled in via this crate's `upload` feature (no bare
// `serde_json` feature exists — the `dep:serde_json` syntax in `[features]`
// suppresses the implicit one), so these tests need it explicitly enabled.
#[cfg(all(test, feature = "upload"))]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_parses_with_defaults() {
        let cfg: UploadConfig =
            serde_json::from_str(r#"{ "path": "/upload", "dir": "./uploads" }"#)
                .expect("minimal config with only path+dir must parse");
        assert_eq!(cfg.path, "/upload");
        assert_eq!(cfg.dir, "./uploads");
        assert_eq!(cfg.max_file_size_bytes, None);
        assert_eq!(cfg.max_total_size_bytes, None);
        assert_eq!(cfg.max_files, None);
        assert_eq!(cfg.allowed_mime_types, None);
        assert_eq!(cfg.field_name, None);
    }

    #[test]
    fn full_config_round_trips_through_json() {
        let cfg = UploadConfig {
            path: "/upload".to_string(),
            dir: "./uploads".to_string(),
            max_file_size_bytes: Some(1_048_576),
            max_total_size_bytes: Some(10_485_760),
            max_files: Some(5),
            allowed_mime_types: Some(vec!["image/".to_string()]),
            field_name: Some("file".to_string()),
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        let round_tripped: UploadConfig =
            serde_json::from_str(&json).expect("deserialize round-trip");
        assert_eq!(cfg, round_tripped);
    }

    /// Regression test for `#[serde(skip_serializing_if = "Option::is_none")]`
    /// on the five optional fields: a config-round-trip test alone would not
    /// catch this attribute being dropped, since `"field": null` round-trips
    /// back to `None` just as well as an absent key does. Assert the keys are
    /// actually missing from the serialized output, not just that they
    /// deserialize back to `None`.
    #[test]
    fn none_optional_fields_are_omitted_from_serialized_json() {
        let cfg = UploadConfig {
            path: "/upload".to_string(),
            dir: "./uploads".to_string(),
            max_file_size_bytes: None,
            max_total_size_bytes: None,
            max_files: None,
            allowed_mime_types: None,
            field_name: None,
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(!json.contains("maxFileSizeBytes"), "got: {json}");
        assert!(!json.contains("maxTotalSizeBytes"), "got: {json}");
        assert!(!json.contains("maxFiles"), "got: {json}");
        assert!(!json.contains("allowedMimeTypes"), "got: {json}");
        assert!(!json.contains("fieldName"), "got: {json}");
    }
}
