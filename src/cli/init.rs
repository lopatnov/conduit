use std::path::Path;

use dialoguer::{Confirm, Input, Select};
use serde_json::{json, Value};

// ── Public options struct ──────────────────────────────────────────────────────

/// All parameters that can be supplied via CLI flags to `conduit init`.
///
/// Any field left as `None` / `false` is either prompted interactively (default
/// mode) or replaced by the built-in default (when `yes == true`).
pub struct InitOptions<'a> {
    /// `-o` — explicit output path; `None` → derive from format choice
    pub output: Option<&'a str>,
    /// `-y` — accept all defaults, no interactive prompts
    pub yes: bool,
    /// `--format` — "yaml" or "json"; `None` → prompt or infer from extension
    pub format: Option<&'a str>,
    /// `--port` — port to listen on
    pub port: Option<u16>,
    /// `--static-dir` — static files directory
    pub static_dir: Option<&'a str>,
    /// `--no-static` — disable static file serving
    pub no_static: bool,
    /// `--proxy` — upstream URL
    pub proxy: Option<&'a str>,
    /// `--no-proxy` — disable proxy
    pub no_proxy: bool,
    /// `--log` — log format: "dev" | "json" | "combined" | "none"
    pub log: Option<&'a str>,
    /// `--no-health` — disable /__health__ endpoint
    pub no_health: bool,
    /// `--tls-cert` — TLS certificate file (manual mode)
    pub tls_cert: Option<&'a str>,
    /// `--tls-key` — TLS private key file (manual mode)
    pub tls_key: Option<&'a str>,
    /// `--tls-acme` — ACME email (auto-TLS mode)
    pub tls_acme: Option<&'a str>,
}

// ── Format ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Yaml,
    Json,
}

impl Format {
    fn default_filename(self) -> &'static str {
        match self {
            Format::Yaml => "conduit.yaml",
            Format::Json => "conduit.json",
        }
    }

    fn from_path(path: &str) -> Option<Self> {
        let lower = path.to_lowercase();
        if lower.ends_with(".yaml") || lower.ends_with(".yml") {
            Some(Format::Yaml)
        } else if lower.ends_with(".json") {
            Some(Format::Json)
        } else {
            None
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "yaml" | "yml" => Some(Format::Yaml),
            "json" => Some(Format::Json),
            _ => None,
        }
    }
}

// ── TLS helper ────────────────────────────────────────────────────────────────

fn ask_tls_config(opts: &InitOptions<'_>) -> anyhow::Result<Option<Value>> {
    // Flag-driven: cert+key → manual TLS; acme → ACME TLS.
    if let (Some(cert), Some(key)) = (opts.tls_cert, opts.tls_key) {
        return Ok(Some(json!({ "cert": cert, "key": key })));
    }
    if let Some(email) = opts.tls_acme {
        return Ok(Some(json!({ "acme": { "email": email } })));
    }

    // Non-interactive default: no TLS.
    if opts.yes {
        return Ok(None);
    }

    // Interactive.
    let want_tls = Confirm::new()
        .with_prompt("Enable TLS (HTTPS)?")
        .default(false)
        .interact()?;

    if !want_tls {
        return Ok(None);
    }

    let tls_options = ["Manual certificate files", "Auto (Let's Encrypt / ACME)"];
    let tls_choice = Select::new()
        .with_prompt("TLS mode")
        .items(tls_options)
        .default(0)
        .interact()?;

    if tls_choice == 0 {
        let cert: String = Input::new()
            .with_prompt("Certificate file path")
            .default("./certs/cert.pem".to_owned())
            .interact_text()?;
        let key: String = Input::new()
            .with_prompt("Private key file path")
            .default("./certs/key.pem".to_owned())
            .interact_text()?;
        Ok(Some(json!({ "cert": cert, "key": key })))
    } else {
        let email: String = Input::new()
            .with_prompt("ACME account email")
            .interact_text()?;
        Ok(Some(json!({ "acme": { "email": email } })))
    }
}

// ── YAML serialization ────────────────────────────────────────────────────────

fn to_yaml_string(value: &Value) -> anyhow::Result<String> {
    Ok(serde_yaml::to_string(value)?)
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Run the `conduit init` wizard with the given options.
///
/// In non-interactive mode (`yes == true`) every unspecified option uses its
/// built-in default. Any flag that was explicitly set takes priority regardless
/// of the `yes` flag.
pub fn run_init(opts: InitOptions<'_>) -> anyhow::Result<()> {
    // ── Format ───────────────────────────────────────────────────────────────
    // Warn when --format is provided but not recognised (mirrors the --log
    // behaviour that warns and falls back to dev).
    if let Some(fmt_str) = opts.format {
        if Format::from_str(fmt_str).is_none() {
            eprintln!(
                "warning: unknown output format '{fmt_str}' — falling back to yaml. \
                 Valid values: yaml, json"
            );
        }
    }
    let format = opts
        .format
        .and_then(Format::from_str)
        .or_else(|| opts.output.and_then(Format::from_path))
        .unwrap_or_else(|| {
            if opts.yes {
                // Non-interactive default: YAML (recommended format).
                Format::Yaml
            } else {
                let format_options = [
                    "YAML  (recommended — supports comments)",
                    "JSON  (compatible with all JSON tooling)",
                ];
                let choice = Select::new()
                    .with_prompt("Output format")
                    .items(format_options)
                    .default(0)
                    .interact()
                    .unwrap_or(0);
                if choice == 0 {
                    Format::Yaml
                } else {
                    Format::Json
                }
            }
        });

    if !opts.yes {
        println!("Welcome to conduit init! Answer a few questions to get started.\n");
    }

    let resolved_path = opts
        .output
        .map(str::to_owned)
        .unwrap_or_else(|| format.default_filename().to_owned());
    let path = Path::new(&resolved_path);

    if path.exists() && !opts.yes {
        let overwrite = Confirm::new()
            .with_prompt(format!("{resolved_path} already exists. Overwrite?"))
            .default(false)
            .interact()?;
        if !overwrite {
            println!("Aborted.");
            return Ok(());
        }
    }

    // ── Port ─────────────────────────────────────────────────────────────────
    let port: u16 = if let Some(p) = opts.port {
        p
    } else if opts.yes {
        8080
    } else {
        Input::new()
            .with_prompt("Port")
            .default(8080u16)
            .interact_text()?
    };

    // ── Static files ─────────────────────────────────────────────────────────
    let static_dir: Option<String> = if opts.no_static {
        None
    } else if let Some(dir) = opts.static_dir {
        Some(dir.to_owned())
    } else if opts.yes {
        // Default: serve ./dist
        Some("./dist".to_owned())
    } else {
        let want = Confirm::new()
            .with_prompt("Serve static files?")
            .default(true)
            .interact()?;
        if want {
            let dir: String = Input::new()
                .with_prompt("Static files directory")
                .default("./dist".to_owned())
                .interact_text()?;
            Some(dir)
        } else {
            None
        }
    };

    // ── Proxy ────────────────────────────────────────────────────────────────
    let proxy_url: Option<String> = if opts.no_proxy {
        None
    } else if let Some(url) = opts.proxy {
        Some(url.to_owned())
    } else if opts.yes {
        // Default: no proxy
        None
    } else {
        let want = Confirm::new()
            .with_prompt("Proxy to an upstream server?")
            .default(false)
            .interact()?;
        if want {
            let url: String = Input::new()
                .with_prompt("Upstream URL")
                .default("http://localhost:4000".to_owned())
                .interact_text()?;
            Some(url)
        } else {
            None
        }
    };

    // ── TLS ──────────────────────────────────────────────────────────────────
    let tls_config = ask_tls_config(&opts)?;

    // ── Health check ─────────────────────────────────────────────────────────
    let want_health: bool = if opts.no_health {
        false
    } else if opts.yes {
        true
    } else {
        Confirm::new()
            .with_prompt("Enable health check endpoint (GET /__health__)?")
            .default(true)
            .interact()?
    };

    // ── Logging ──────────────────────────────────────────────────────────────
    let logging_value: Option<Value> = if let Some(fmt) = opts.log {
        match fmt {
            "dev" => Some(json!("dev")),
            "json" => Some(json!("json")),
            "combined" => Some(json!("combined")),
            "none" => None,
            other => {
                eprintln!("warning: unknown log format '{other}', defaulting to 'dev'");
                Some(json!("dev"))
            }
        }
    } else if opts.yes {
        Some(json!("dev"))
    } else {
        let log_formats = [
            "dev (human-readable)",
            "json (structured)",
            "combined (Apache)",
            "none",
        ];
        let choice = Select::new()
            .with_prompt("Log format")
            .items(log_formats)
            .default(0)
            .interact()?;
        match choice {
            0 => Some(json!("dev")),
            1 => Some(json!("json")),
            2 => Some(json!("combined")),
            _ => None,
        }
    };

    // ── Assemble ─────────────────────────────────────────────────────────────
    let mut site = json!({ "port": port });

    if let Some(dir) = static_dir {
        site["static"] = json!(dir);
    }
    if let Some(url) = proxy_url {
        site["proxy"] = json!(url);
    }
    if let Some(tls) = tls_config {
        site["tls"] = tls;
    }
    if want_health {
        site["healthCheck"] = json!(true);
    }
    if let Some(logging) = logging_value {
        site["logging"] = logging;
    }

    // ── Serialize ─────────────────────────────────────────────────────────────
    let config_str = match format {
        Format::Yaml => to_yaml_string(&site)?,
        Format::Json => serde_json::to_string_pretty(&site)?,
    };

    std::fs::write(path, &config_str)?;

    if opts.yes {
        println!("Wrote {resolved_path}");
    } else {
        println!("\nWrote {resolved_path}:\n");
        println!("{config_str}");
        println!(
            "Run `conduit -c {resolved_path}` to start, or \
             `conduit validate -c {resolved_path}` to check the config."
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── Format helpers ────────────────────────────────────────────────────────

    #[test]
    fn format_from_path_yaml_extension() {
        assert!(matches!(
            Format::from_path("conduit.yaml"),
            Some(Format::Yaml)
        ));
        assert!(matches!(
            Format::from_path("config.yml"),
            Some(Format::Yaml)
        ));
    }

    #[test]
    fn format_from_path_json_extension() {
        assert!(matches!(
            Format::from_path("conduit.json"),
            Some(Format::Json)
        ));
    }

    #[test]
    fn format_from_path_uppercase_extension() {
        // Case-insensitive matching.
        assert!(matches!(
            Format::from_path("config.YAML"),
            Some(Format::Yaml)
        ));
        assert!(matches!(
            Format::from_path("config.JSON"),
            Some(Format::Json)
        ));
    }

    #[test]
    fn format_from_path_unknown_extension_returns_none() {
        assert!(Format::from_path("config.toml").is_none());
        assert!(Format::from_path("config").is_none());
    }

    #[test]
    fn format_from_str_yaml_variants() {
        assert!(matches!(Format::from_str("yaml"), Some(Format::Yaml)));
        assert!(matches!(Format::from_str("yml"), Some(Format::Yaml)));
        assert!(matches!(Format::from_str("YAML"), Some(Format::Yaml)));
    }

    #[test]
    fn format_from_str_json() {
        assert!(matches!(Format::from_str("json"), Some(Format::Json)));
        assert!(matches!(Format::from_str("JSON"), Some(Format::Json)));
    }

    #[test]
    fn format_from_str_unknown_returns_none() {
        assert!(Format::from_str("toml").is_none());
        assert!(Format::from_str("").is_none());
    }

    #[test]
    fn format_default_filename() {
        assert_eq!(Format::Yaml.default_filename(), "conduit.yaml");
        assert_eq!(Format::Json.default_filename(), "conduit.json");
    }

    // ── to_yaml_string ────────────────────────────────────────────────────────

    #[test]
    fn to_yaml_string_simple_value() {
        let v = serde_json::json!({ "port": 8080 });
        let yaml = to_yaml_string(&v).expect("yaml serialization");
        assert!(yaml.contains("port"), "yaml must contain key 'port'");
        assert!(yaml.contains("8080"), "yaml must contain value 8080");
    }

    // ── run_init non-interactive ──────────────────────────────────────────────

    #[test]
    fn run_init_yes_writes_yaml() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("conduit.yaml");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("yaml"),
            port: Some(9090),
            static_dir: None,
            no_static: true,
            proxy: Some("http://upstream:4000"),
            no_proxy: false,
            log: Some("dev"),
            no_health: false,
            tls_cert: None,
            tls_key: None,
            tls_acme: None,
        };
        run_init(opts).expect("run_init must succeed");
        let content = std::fs::read_to_string(&output).expect("output must exist");
        assert!(content.contains("9090"), "port must appear in output");
        assert!(
            content.contains("upstream:4000"),
            "proxy must appear in output"
        );
    }

    #[test]
    fn run_init_yes_writes_json() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("conduit.json");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("json"),
            port: Some(3000),
            static_dir: None,
            no_static: true,
            proxy: None,
            no_proxy: true,
            log: Some("json"),
            no_health: true,
            tls_cert: None,
            tls_key: None,
            tls_acme: None,
        };
        run_init(opts).expect("run_init must succeed");
        let content = std::fs::read_to_string(&output).expect("output must exist");
        // Should be valid JSON.
        let parsed: serde_json::Value =
            serde_json::from_str(&content).expect("output must be valid JSON");
        assert_eq!(parsed["port"], 3000);
    }

    #[test]
    fn run_init_with_tls_cert_key() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("conduit.yaml");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("yaml"),
            port: Some(443),
            static_dir: None,
            no_static: true,
            proxy: Some("http://backend:4000"),
            no_proxy: false,
            log: Some("combined"),
            no_health: false,
            tls_cert: Some("./certs/server.pem"),
            tls_key: Some("./certs/server.key"),
            tls_acme: None,
        };
        run_init(opts).expect("run_init with TLS must succeed");
        let content = std::fs::read_to_string(&output).expect("output must exist");
        assert!(
            content.contains("server.pem"),
            "TLS cert must appear in output"
        );
        assert!(
            content.contains("server.key"),
            "TLS key must appear in output"
        );
    }

    #[test]
    fn run_init_log_format_unknown_falls_back_to_dev() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("conduit.yaml");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("yaml"),
            port: Some(8080),
            static_dir: None,
            no_static: true,
            proxy: None,
            no_proxy: true,
            log: Some("unknown-format"),
            no_health: true,
            tls_cert: None,
            tls_key: None,
            tls_acme: None,
        };
        // Unknown log format should not panic; falls back to "dev".
        run_init(opts).expect("unknown log format must not error");
    }

    #[test]
    fn run_init_unknown_format_flag_falls_back_to_yaml() {
        // An unrecognised --format value warns (mirroring --log behavior) and
        // falls back to YAML instead of silently dropping the input.
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("conduit.yaml");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("xml"), // unknown format
            port: Some(8080),
            static_dir: None,
            no_static: true,
            proxy: None,
            no_proxy: true,
            log: Some("dev"),
            no_health: true,
            tls_cert: None,
            tls_key: None,
            tls_acme: None,
        };
        // Must not error — falls back gracefully to yaml.
        run_init(opts).expect("unknown format must not error");
        // Output file should exist (written as yaml since output has .yaml extension).
        assert!(
            output.exists(),
            "output file must be written even with unknown format"
        );
    }

    #[test]
    fn run_init_with_acme_tls() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("conduit.yaml");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("yaml"),
            port: Some(443),
            static_dir: None,
            no_static: true,
            proxy: None,
            no_proxy: true,
            log: Some("dev"),
            no_health: false,
            tls_cert: None,
            tls_key: None,
            tls_acme: Some("admin@example.com"),
        };
        run_init(opts).expect("ACME TLS init must succeed");
        let content = std::fs::read_to_string(&output).unwrap();
        assert!(
            content.contains("admin@example.com"),
            "ACME email must appear in output: {content}"
        );
    }

    #[test]
    fn run_init_with_static_dir() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("conduit.yaml");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("yaml"),
            port: Some(8080),
            static_dir: Some("./public"),
            no_static: false,
            proxy: None,
            no_proxy: true,
            log: Some("dev"),
            no_health: false,
            tls_cert: None,
            tls_key: None,
            tls_acme: None,
        };
        run_init(opts).expect("init with static dir must succeed");
        let content = std::fs::read_to_string(&output).unwrap();
        assert!(
            content.contains("public"),
            "static dir must appear in output"
        );
    }

    #[test]
    fn run_init_log_none_omits_logging() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("conduit.yaml");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("yaml"),
            port: Some(8080),
            static_dir: None,
            no_static: true,
            proxy: None,
            no_proxy: true,
            log: Some("none"),
            no_health: true,
            tls_cert: None,
            tls_key: None,
            tls_acme: None,
        };
        run_init(opts).expect("log: none must not error");
        let content = std::fs::read_to_string(&output).unwrap();
        // When logging is "none", no logging key should appear.
        assert!(
            !content.contains("logging"),
            "log:none must omit logging key"
        );
    }

    #[test]
    fn run_init_log_combined_format() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("conduit.yaml");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("yaml"),
            port: Some(8080),
            static_dir: None,
            no_static: true,
            proxy: None,
            no_proxy: true,
            log: Some("combined"),
            no_health: true,
            tls_cert: None,
            tls_key: None,
            tls_acme: None,
        };
        run_init(opts).expect("log: combined must not error");
        let content = std::fs::read_to_string(&output).unwrap();
        assert!(
            content.contains("combined"),
            "log:combined must appear in output"
        );
    }

    #[test]
    fn run_init_default_yes_uses_defaults() {
        // With yes=true and no explicit options, should use defaults:
        // port=8080, static=./dist, log=dev, health=true
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("out.yaml");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: Some("yaml"),
            port: None, // should default to 8080
            static_dir: None,
            no_static: false, // should use default ./dist
            proxy: None,
            no_proxy: true,
            log: None, // should default to "dev"
            no_health: false,
            tls_cert: None,
            tls_key: None,
            tls_acme: None,
        };
        run_init(opts).expect("default yes must succeed");
        let content = std::fs::read_to_string(&output).unwrap();
        assert!(content.contains("8080"), "default port must be 8080");
    }
}
