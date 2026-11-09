use std::path::Path;

use anyhow::{Context, Result};
use conduit_config_core::format::ConfigFormat;
use conduit_config_core::parse::{from_json_str, from_json_str_verbatim, from_yaml_str};

use crate::schema::{AppConfig, ConfigFile, SiteConfig};

/// Load and parse a config file from disk, performing env interpolation first.
///
/// Both JSON (`.json`) and YAML (`.yaml` / `.yml`) are supported.
/// The format is determined by the file extension; JSON is the default.
pub fn load_config(path: &Path) -> Result<AppConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read config file: {}", path.display()))?;
    match ConfigFormat::from_path(path) {
        ConfigFormat::Yaml => from_yaml(&raw),
        ConfigFormat::Json => from_str(&raw),
    }
    .with_context(|| format!("Cannot parse config file: {}", path.display()))
}

/// Parse a config JSON string, performing env interpolation first.
pub fn from_str(text: &str) -> Result<AppConfig> {
    let file: ConfigFile = from_json_str(text).map_err(|e| diagnose(text, false, e))?;
    reject_failed_full(&file, text, false)?;
    let ignored = ignored_keys(text, false, &file);
    let mut app = normalize(file);
    app.ignored_keys = ignored;
    Ok(app)
}

/// Parse a config YAML string, performing env interpolation first.
pub fn from_yaml(text: &str) -> Result<AppConfig> {
    let file: ConfigFile = from_yaml_str(text).map_err(|e| diagnose(text, true, e))?;
    reject_failed_full(&file, text, true)?;
    let ignored = ignored_keys(text, true, &file);
    let mut app = normalize(file);
    app.ignored_keys = ignored;
    Ok(app)
}

/// The keys of the document that the parsed configuration does not read, as paths (`sites[0].cors.origns`): a
/// mistyped option is dropped by serde without a word and the default stays in force (issue #682).
///
/// Found without touching the structures: the parsed `file` is written back to a JSON value and compared with the
/// document; a key that is in the document and not in what was written back was not read. Only objects are
/// compared with objects and arrays with arrays (a shorthand such as `cors: true` has no keys to compare), and a key
/// whose value is `null` or `false` is never reported: `null` is the same as no key, and `false` is what a plain
/// `bool` that is omitted when false (`tls.clientAuth.optional`) looks like. A key that the structure itself keeps
/// (`SiteConfig.extra`, `GlobalConfig.extra`) is written back, so it is not reported here; the site-level and
/// `global` warnings of `validate` name those.
fn ignored_keys(text: &str, yaml: bool, file: &ConfigFile) -> Vec<String> {
    let raw: Result<serde_json::Value> = if yaml {
        from_yaml_str(text)
    } else {
        from_json_str(text)
    };
    let (Ok(raw), Ok(back)) = (raw, serde_json::to_value(file)) else {
        return Vec::new();
    };
    let root = match file {
        ConfigFile::Full(_) => "",
        ConfigFile::Sites(_) => "sites",
        ConfigFile::Single(_) => "sites[0]",
    };
    let mut out = Vec::new();
    collect_ignored(&raw, &back, root, true, &mut out);
    out
}

fn collect_ignored(
    raw: &serde_json::Value,
    back: &serde_json::Value,
    path: &str,
    top: bool,
    out: &mut Vec<String>,
) {
    use serde_json::Value;
    match (raw, back) {
        (Value::Object(r), Value::Object(b)) => {
            for (key, value) in r {
                // the editor hint and the schema version are keys of the top level that have no field
                if top && (key == "$schema" || key == "version") {
                    continue;
                }
                let here = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match b.get(key) {
                    Some(kept) => collect_ignored(value, kept, &here, false, out),
                    None if value.is_null() || *value == Value::Bool(false) => {}
                    None => out.push(here),
                }
            }
        }
        (Value::Array(r), Value::Array(b)) => {
            for (i, (value, kept)) in r.iter().zip(b).enumerate() {
                collect_ignored(value, kept, &format!("{path}[{i}]"), false, out);
            }
        }
        _ => {}
    }
}

/// `ConfigFile` is an untagged enum, so when no variant matches serde can only say "data did not
/// match any variant" and `serde_path_to_error` loses the field path (issue #570). Pick the variant
/// the document's shape implies (array → sites, `sites` key → full, else single site) and parse into
/// that concrete type, whose error names the field. The variant order of `ConfigFile` is unchanged.
fn diagnose(text: &str, yaml: bool, original: anyhow::Error) -> anyhow::Error {
    if !original.to_string().starts_with("Config parse error") {
        return original;
    }
    let value: Result<serde_json::Value> = if yaml {
        from_yaml_str(text)
    } else {
        from_json_str(text)
    };
    let Ok(value) = value else {
        return original;
    };
    // `value` is already interpolated: the re-parse must not expand a `$NAME` that an environment value contains
    // (issue #586).
    let json = value.to_string();
    let refined = if value.is_array() {
        from_json_str_verbatim::<Vec<SiteConfig>>(&json).err()
    } else if value.get("sites").is_some() {
        from_json_str_verbatim::<AppConfig>(&json).err()
    } else {
        from_json_str_verbatim::<SiteConfig>(&json).err()
    };
    match refined {
        Some(e) => anyhow::anyhow!(redact_value(&e.to_string())),
        // the first error quotes the rejected value as well; keep it out of the message here too
        None => anyhow::anyhow!(redact_value(&original.to_string())),
    }
}

/// Serde's type errors quote the rejected value (`invalid type: string "hunter2", expected u64`);
/// after env interpolation that value may be a secret, so keep the field path and the expected
/// type and drop the value from the diagnostic.
fn redact_value(msg: &str) -> String {
    for lead in ["invalid type: ", "invalid value: "] {
        if let Some(i) = msg.find(lead) {
            let (head, rest) = msg.split_at(i + lead.len());
            let tail = rest.find(", expected ").map(|j| &rest[j..]).unwrap_or("");
            let kind = rest.split([' ', ',']).next().unwrap_or("value");
            return format!("{head}{kind}{tail}");
        }
    }
    // `unknown variant `value`, expected one of ...` quotes the rejected value too
    if let Some(i) = msg.find("unknown variant `") {
        let start = i + "unknown variant `".len();
        if let Some(len) = msg[start..].find("`, expected ") {
            return format!("{}unknown variant{}", &msg[..i], &msg[start + len + 1..]);
        }
    }
    msg.to_owned()
}

/// `Single` accepts anything, so a full config (`sites: [...]`) whose sites fail to parse used to
/// fall through to it and load as one empty site with `sites` parked in `extra`. Report the real
/// error instead (issue #570).
fn reject_failed_full(file: &ConfigFile, text: &str, yaml: bool) -> Result<()> {
    if let ConfigFile::Single(site) = file {
        if site.extra.contains_key("sites") || site.extra.contains_key("global") {
            let value: Result<serde_json::Value> = if yaml {
                from_yaml_str(text)
            } else {
                from_json_str(text)
            };
            value
                .and_then(|v| from_json_str_verbatim::<AppConfig>(&v.to_string()))
                .map_err(|e| anyhow::anyhow!(redact_value(&e.to_string())))?;
        }
    }
    Ok(())
}

/// Normalize all ConfigFile variants into a canonical AppConfig.
pub fn normalize(file: ConfigFile) -> AppConfig {
    match file {
        ConfigFile::Full(app) => *app,
        ConfigFile::Sites(sites) => AppConfig {
            global: None,
            sites,
            ..Default::default()
        },
        ConfigFile::Single(site) => AppConfig {
            global: None,
            sites: vec![*site],
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── keys the parser did not read (issue #682) ───────────────────────────

    fn ignored(json: &str) -> Vec<String> {
        from_str(json).unwrap().ignored_keys
    }

    #[test]
    fn a_mistyped_key_in_a_nested_object_is_listed_with_its_path() {
        // a single-site document, a full one and a list of sites
        assert_eq!(
            ignored(r#"{ "port": 8080, "cors": { "origns": ["https://a.example"] } }"#),
            ["sites[0].cors.origns"]
        );
        assert_eq!(
            ignored(
                r#"{ "sites": [{ "port": 1 }, { "port": 2, "securityHeaders": { "frameOptions": "DENY" } }] }"#
            ),
            ["sites[1].securityHeaders.frameOptions"]
        );
        assert_eq!(
            ignored(r#"[{ "port": 1, "hotReload": { "watch": ["./src"] } }]"#),
            ["sites[0].hotReload.watch"]
        );
    }

    #[test]
    fn a_mistyped_key_in_a_route_and_in_its_health_check_is_listed() {
        let keys = ignored(
            r#"{ "port": 8080, "proxy": { "/api": {
                   "targets": ["http://a:1"],
                   "maxConnectionsPerUpstream": 5,
                   "healthCheck": { "path": "/h", "timeoutSec": 2 } } } }"#,
        );
        assert_eq!(
            keys,
            [
                "sites[0].proxy./api.healthCheck.timeoutSec",
                "sites[0].proxy./api.maxConnectionsPerUpstream"
            ]
        );
    }

    #[test]
    fn a_correct_configuration_lists_nothing() {
        let keys = ignored(
            r#"{ "version": 1, "$schema": "x", "global": { "workers": 2 },
                 "sites": [{ "port": 8080, "cors": { "origins": ["https://a.example"], "allowedHeaders": ["X"] },
                             "securityHeaders": { "hstsMaxAgeSecs": 1, "csp": "default-src 'self'" },
                             "hotReload": { "extensions": ["css"] },
                             "proxy": { "/api": { "targets": ["http://a:1"],
                                                  "healthCheck": { "path": "/h", "timeoutMs": 500,
                                                                   "maxConnectionsPerUpstream": 5 } } } }] }"#,
        );
        assert!(keys.is_empty(), "{keys:?}");
    }

    #[test]
    fn null_false_and_shorthand_values_are_not_reported() {
        // `null` is the same as no key; `false` is what a plain bool omitted when false looks like
        // (tls.clientAuth.optional); a shorthand has no keys to compare
        let keys = ignored(
            r#"{ "port": 8080, "cors": true, "logging": "json", "compression": { "level": null },
                 "tls": { "cert": "a.pem", "key": "b.pem", "clientAuth": { "ca": "c.pem", "optional": false } } }"#,
        );
        assert!(keys.is_empty(), "{keys:?}");
    }

    #[test]
    fn the_same_keys_are_found_in_a_yaml_file() {
        let app = from_yaml(
            "port: 8080
cors:
  origns: [\"https://a.example\"]
securityHeaders:
  hsts: x
",
        )
        .unwrap();
        assert_eq!(
            app.ignored_keys,
            ["sites[0].cors.origns", "sites[0].securityHeaders.hsts"]
        );
    }

    #[test]
    fn diagnostics_do_not_echo_the_rejected_value() {
        let e = from_str(r#"{ "port": "hunter2-secret" }"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("port") && !e.contains("hunter2"), "{e}");
        let e = from_str(r#"{ "sites": [{ "port": "hunter2-secret" }] }"#)
            .unwrap_err()
            .to_string();
        assert!(!e.contains("hunter2"), "{e}");
    }

    /// Issue #586: the rejected value of an enum field is not echoed either (serde quotes it in backticks).
    #[test]
    fn an_unknown_variant_is_redacted_without_its_value() {
        let msg = "Config parse error at 'sites[0].tls.versions[0]': unknown variant `hunter2-secret`, expected one of `1.2`, `1.3` at line 1 column 9";
        let out = redact_value(msg);
        assert!(!out.contains("hunter2"), "{out}");
        assert!(
            out.contains("unknown variant, expected one of `1.2`, `1.3`")
                && out.contains("tls.versions[0]"),
            "{out}"
        );
        // a message without a quoted value is left alone
        assert_eq!(
            redact_value("missing field `sites`"),
            "missing field `sites`"
        );
    }

    #[test]
    fn global_without_sites_is_an_error_not_an_empty_site() {
        let e = from_str(r#"{ "global": { "workers": 4 } }"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("sites"), "{e}");
    }

    /// Issue #570: a wrong-typed value is reported with its field path, in every config shape.
    #[test]
    fn type_error_names_the_field() {
        let single = r#"{ "port": 8080, "rateLimit": { "windowSecs": "soon", "limit": 5 } }"#;
        let e = from_str(single).unwrap_err().to_string();
        assert!(e.contains("rateLimit.windowSecs"), "single: {e}");

        let sites = r#"{ "sites": [{ "port": 8080 }, { "port": "http" }] }"#;
        let e = from_str(sites).unwrap_err().to_string();
        assert!(e.contains("sites[1].port"), "sites: {e}");

        let array = r#"[{ "port": 1 }, { "port": "x" }]"#;
        let e = from_str(array).unwrap_err().to_string();
        assert!(e.contains("[1].port"), "array: {e}");

        let yaml = "port: 8080\nrateLimit: { windowSecs: soon, limit: 5 }\n";
        let e = from_yaml(yaml).unwrap_err().to_string();
        assert!(e.contains("rateLimit.windowSecs"), "yaml: {e}");
    }
    use crate::schema::SiteConfig;

    // ── load_config ───────────────────────────────────────────────────────────

    #[test]
    fn load_config_parses_valid_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.json");
        std::fs::write(&path, r#"{"port": 8080}"#).unwrap();
        assert!(load_config(&path).is_ok());
    }

    #[test]
    fn load_config_parses_valid_yaml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.yaml");
        std::fs::write(&path, "port: 8080\n").unwrap();
        assert!(load_config(&path).is_ok());
    }

    #[test]
    fn load_config_parses_yml_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.yml");
        std::fs::write(&path, "port: 3000\n").unwrap();
        let cfg = load_config(&path).expect("should parse .yml");
        assert_eq!(cfg.sites[0].port, Some(3000));
    }

    #[test]
    fn load_config_missing_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("__conduit_test_missing__.json");
        assert!(load_config(&missing).is_err());
    }

    // ── from_yaml ─────────────────────────────────────────────────────────────

    #[test]
    fn yaml_single_site_form() {
        let cfg = from_yaml("port: 8080\n").expect("parse");
        assert_eq!(cfg.sites[0].port, Some(8080));
    }

    #[test]
    fn yaml_sites_array_form() {
        let yaml = "- port: 8080\n- port: 8081\n";
        let cfg = from_yaml(yaml).expect("parse");
        assert_eq!(cfg.sites.len(), 2);
        assert_eq!(cfg.sites[0].port, Some(8080));
        assert_eq!(cfg.sites[1].port, Some(8081));
    }

    #[test]
    fn yaml_full_form_with_global() {
        let yaml = "global:\n  workers: 4\nsites:\n  - port: 9000\n";
        let cfg = from_yaml(yaml).expect("parse");
        assert_eq!(cfg.sites[0].port, Some(9000));
        assert!(cfg.global.is_some());
    }

    #[test]
    fn yaml_invalid_port_type_returns_error() {
        let result = from_yaml("port: \"not-a-number\"\n");
        assert!(result.is_err(), "invalid port type must fail");
    }

    #[test]
    fn yaml_unsupported_version_returns_error() {
        let result = from_yaml("version: 999\nport: 8080\n");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("999") || msg.contains("version") || msg.contains("upgrade"));
    }

    // ── from_str (JSON) ───────────────────────────────────────────────────────

    #[test]
    fn unsupported_version_returns_error() {
        let result = from_str(r#"{"version": 999, "port": 8080}"#);
        assert!(result.is_err(), "version 999 must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("999") || msg.contains("version") || msg.contains("upgrade"),
            "error message should mention version: {msg}"
        );
    }

    #[test]
    fn supported_version_parses_ok() {
        assert!(from_str(r#"{"version": 1, "port": 8080}"#).is_ok());
    }

    #[test]
    fn no_version_field_parses_ok() {
        assert!(from_str(r#"{"port": 8080}"#).is_ok());
    }

    #[test]
    fn invalid_json_returns_error() {
        let result = from_str(r#"{"port": "not-a-number"}"#);
        assert!(result.is_err(), "invalid port type must fail to parse");
    }

    #[test]
    fn normalize_full_config_returns_sites() {
        use crate::schema::GlobalConfig;
        let app = AppConfig {
            global: Some(GlobalConfig {
                workers: Some(4),
                ..Default::default()
            }),
            sites: vec![SiteConfig::default()],
            ..Default::default()
        };
        let out = normalize(ConfigFile::Full(Box::new(app.clone())));
        assert_eq!(out.sites.len(), 1);
        assert!(out.global.is_some());
    }

    #[test]
    fn normalize_single_site_wraps_in_vec() {
        let out = normalize(ConfigFile::Single(Box::default()));
        assert_eq!(out.sites.len(), 1);
        assert!(out.global.is_none());
    }

    // ── from_yaml ─────────────────────────────────────────────────────────────

    #[test]
    fn from_yaml_parses_simple_config() {
        let result = from_yaml("port: 9090\n");
        assert!(result.is_ok(), "valid YAML must parse: {:?}", result);
        let cfg = result.unwrap();
        assert_eq!(cfg.sites[0].port, Some(9090));
    }

    #[test]
    fn from_yaml_version_too_new_rejected() {
        let result = from_yaml("version: 999\nport: 8080\n");
        assert!(result.is_err(), "YAML version 999 must be rejected");
    }

    // ── normalize ─────────────────────────────────────────────────────────────

    #[test]
    fn normalize_sites_array_preserves_order() {
        let sites = vec![
            SiteConfig {
                port: Some(8080),
                ..Default::default()
            },
            SiteConfig {
                port: Some(8081),
                ..Default::default()
            },
        ];
        let out = normalize(ConfigFile::Sites(sites));
        assert_eq!(out.sites[0].port, Some(8080));
        assert_eq!(out.sites[1].port, Some(8081));
    }
}
