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

    /// Serialize `value` in this format.
    fn serialize(self, value: &Value) -> anyhow::Result<String> {
        match self {
            Format::Yaml => to_yaml_string(value),
            Format::Json => Ok(serde_json::to_string_pretty(value)?),
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

// ── Per-field helpers ─────────────────────────────────────────────────────────

fn resolve_format(opts: &InitOptions<'_>) -> Format {
    opts.format
        .and_then(Format::from_str)
        .or_else(|| opts.output.and_then(Format::from_path))
        .unwrap_or_else(|| {
            if opts.yes {
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
        })
}

fn ask_port(opts: &InitOptions<'_>) -> anyhow::Result<u16> {
    if let Some(p) = opts.port {
        return Ok(p);
    }
    if opts.yes {
        return Ok(8080);
    }
    Ok(Input::new()
        .with_prompt("Port")
        .default(8080u16)
        .interact_text()?)
}

fn ask_static_dir(opts: &InitOptions<'_>) -> anyhow::Result<Option<String>> {
    if opts.no_static {
        return Ok(None);
    }
    if let Some(dir) = opts.static_dir {
        return Ok(Some(dir.to_owned()));
    }
    if opts.yes {
        return Ok(Some("./dist".to_owned()));
    }
    let want = Confirm::new()
        .with_prompt("Serve static files?")
        .default(true)
        .interact()?;
    if want {
        let dir: String = Input::new()
            .with_prompt("Static files directory")
            .default("./dist".to_owned())
            .interact_text()?;
        Ok(Some(dir))
    } else {
        Ok(None)
    }
}

fn ask_proxy_url(opts: &InitOptions<'_>) -> anyhow::Result<Option<String>> {
    if opts.no_proxy {
        return Ok(None);
    }
    if let Some(url) = opts.proxy {
        return Ok(Some(url.to_owned()));
    }
    if opts.yes {
        return Ok(None);
    }
    let want = Confirm::new()
        .with_prompt("Proxy to an upstream server?")
        .default(false)
        .interact()?;
    if want {
        let url: String = Input::new()
            .with_prompt("Upstream URL")
            .default("http://localhost:4000".to_owned())
            .interact_text()?;
        Ok(Some(url))
    } else {
        Ok(None)
    }
}

fn ask_health(opts: &InitOptions<'_>) -> anyhow::Result<bool> {
    if opts.no_health {
        return Ok(false);
    }
    if opts.yes {
        return Ok(true);
    }
    Ok(Confirm::new()
        .with_prompt("Enable health check endpoint (GET /__health__)?")
        .default(true)
        .interact()?)
}

fn ask_logging(opts: &InitOptions<'_>) -> anyhow::Result<Option<Value>> {
    if let Some(fmt) = opts.log {
        return Ok(match fmt {
            "dev" => Some(json!("dev")),
            "json" => Some(json!("json")),
            "combined" => Some(json!("combined")),
            "none" => None,
            other => {
                eprintln!("warning: unknown log format '{other}', defaulting to 'dev'");
                Some(json!("dev"))
            }
        });
    }
    if opts.yes {
        return Ok(Some(json!("dev")));
    }
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
    Ok(match choice {
        0 => Some(json!("dev")),
        1 => Some(json!("json")),
        2 => Some(json!("combined")),
        _ => None,
    })
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Warn on stderr when `--format` was given but isn't recognised (falls back
/// to yaml via `resolve_format`).
fn warn_unknown_format(opts: &InitOptions<'_>) {
    let Some(fmt_str) = opts.format else {
        return;
    };
    if Format::from_str(fmt_str).is_none() {
        eprintln!(
            "warning: unknown output format '{fmt_str}' — falling back to yaml. \
             Valid values: yaml, json"
        );
    }
}

/// Prompt to overwrite an existing config file. `Ok(false)` means the caller
/// should abort without writing (already printed "Aborted.").
fn confirm_overwrite(path: &Path, resolved_path: &str, yes: bool) -> anyhow::Result<bool> {
    if !path.exists() || yes {
        return Ok(true);
    }
    let overwrite = Confirm::new()
        .with_prompt(format!("{resolved_path} already exists. Overwrite?"))
        .default(false)
        .interact()?;
    if !overwrite {
        println!("Aborted.");
    }
    Ok(overwrite)
}

/// The six wizard answers, in the order they are prompted.
struct Answers {
    port: u16,
    static_dir: Option<String>,
    proxy_url: Option<String>,
    tls: Option<Value>,
    health: bool,
    logging: Option<Value>,
}

fn collect_answers(opts: &InitOptions<'_>) -> anyhow::Result<Answers> {
    Ok(Answers {
        port: ask_port(opts)?,
        static_dir: ask_static_dir(opts)?,
        proxy_url: ask_proxy_url(opts)?,
        tls: ask_tls_config(opts)?,
        health: ask_health(opts)?,
        logging: ask_logging(opts)?,
    })
}

/// Assemble the site config object; keys are added only when configured.
fn assemble_site(answers: Answers) -> Value {
    let mut site = json!({ "port": answers.port });

    if let Some(dir) = answers.static_dir {
        site["static"] = json!(dir);
    }
    if let Some(url) = answers.proxy_url {
        site["proxy"] = json!(url);
    }
    if let Some(tls) = answers.tls {
        site["tls"] = tls;
    }
    if answers.health {
        site["healthCheck"] = json!(true);
    }
    if let Some(logging) = answers.logging {
        site["logging"] = logging;
    }

    site
}

/// Print the post-write summary (quiet in `-y` mode).
fn print_outcome(yes: bool, resolved_path: &str, config_str: &str) {
    if yes {
        println!("Wrote {resolved_path}");
    } else {
        println!("\nWrote {resolved_path}:\n");
        println!("{config_str}");
        println!(
            "Run `conduit -c {resolved_path}` to start, or \
             `conduit validate -c {resolved_path}` to check the config."
        );
    }
}

/// Run the `conduit init` wizard with the given options.
///
/// In non-interactive mode (`yes == true`) every unspecified option uses its
/// built-in default. Any flag that was explicitly set takes priority regardless
/// of the `yes` flag.
pub fn run_init(opts: InitOptions<'_>) -> anyhow::Result<()> {
    warn_unknown_format(&opts);
    let format = resolve_format(&opts);

    if !opts.yes {
        println!("Welcome to conduit init! Answer a few questions to get started.\n");
    }

    let resolved_path = opts
        .output
        .map(str::to_owned)
        .unwrap_or_else(|| format.default_filename().to_owned());
    let path = Path::new(&resolved_path);

    if !confirm_overwrite(path, &resolved_path, opts.yes)? {
        return Ok(());
    }

    let answers = collect_answers(&opts)?;
    let site = assemble_site(answers);
    let config_str = format.serialize(&site)?;

    std::fs::write(path, &config_str)?;
    print_outcome(opts.yes, &resolved_path, &config_str);

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
        assert!(
            content.contains("dist"),
            "default static dir must be ./dist"
        );
        assert!(content.contains("dev"), "default log format must be dev");
        assert!(
            content.contains("healthCheck"),
            "health check must be enabled by default"
        );
    }

    #[test]
    fn run_init_log_json_format() {
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
            log: Some("json"),
            no_health: false,
            tls_cert: None,
            tls_key: None,
            tls_acme: None,
        };
        run_init(opts).expect("log: json must not error");
        let content = std::fs::read_to_string(&output).unwrap();
        assert!(content.contains("json"), "log: json must appear in output");
    }

    #[test]
    fn run_init_overrides_format_from_output_extension() {
        // When --format is omitted, the output extension determines format.
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("config.json");
        let opts = InitOptions {
            output: Some(output.to_str().unwrap()),
            yes: true,
            format: None, // no explicit format
            port: Some(9000),
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
        run_init(opts).expect("extension-inferred format must succeed");
        // Output must be valid JSON (config.json → JSON format).
        let content = std::fs::read_to_string(&output).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&content).is_ok(),
            "output inferred as JSON must be valid JSON: {content:.50}"
        );
    }
}
