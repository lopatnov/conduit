use std::path::Path;

use dialoguer::{Confirm, Input, Select};
use serde_json::{json, Value};

/// Output format selected by the user.
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

    /// Infer format from a file extension, returning `None` when unknown.
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
}

/// Ask the user how to configure TLS (if at all).
fn ask_tls_config() -> anyhow::Result<Option<Value>> {
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

/// Serialize `value` to a YAML string.
///
/// The output uses the serde_yaml formatter.  Comments from the schema are
/// not included — for a fully-annotated starting point use `examples/minimal.yaml`.
fn to_yaml_string(value: &Value) -> anyhow::Result<String> {
    Ok(serde_yaml::to_string(value)?)
}

/// Run the interactive `conduit init` wizard.
///
/// * `output_path` — explicit output path from `-o`; when `None` the wizard
///   asks for the format and picks a sensible default name (`conduit.yaml` or
///   `conduit.json`).
pub fn run_init(output_path: Option<&str>) -> anyhow::Result<()> {
    println!("Welcome to conduit init! Answer a few questions to get started.\n");

    // ── Format ───────────────────────────────────────────────────────────────
    // If the output path was given explicitly, infer format from extension.
    // Otherwise ask the user.
    let format = if let Some(path) = output_path {
        Format::from_path(path).unwrap_or(Format::Yaml)
    } else {
        let format_options = [
            "YAML  (recommended — supports comments)",
            "JSON  (compatible with all JSON tooling)",
        ];
        let fmt_choice = Select::new()
            .with_prompt("Output format")
            .items(format_options)
            .default(0)
            .interact()?;
        if fmt_choice == 0 { Format::Yaml } else { Format::Json }
    };

    let resolved_path = output_path
        .map(str::to_owned)
        .unwrap_or_else(|| format.default_filename().to_owned());
    let path = Path::new(&resolved_path);

    if path.exists() {
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
    let port: u16 = Input::new()
        .with_prompt("Port")
        .default(8080u16)
        .interact_text()?;

    // ── Static file serving ──────────────────────────────────────────────────
    let want_static = Confirm::new()
        .with_prompt("Serve static files?")
        .default(true)
        .interact()?;

    let static_dir: Option<String> = if want_static {
        let dir: String = Input::new()
            .with_prompt("Static files directory")
            .default("./dist".to_owned())
            .interact_text()?;
        Some(dir)
    } else {
        None
    };

    // ── Reverse proxy ────────────────────────────────────────────────────────
    let want_proxy = Confirm::new()
        .with_prompt("Proxy to an upstream server?")
        .default(false)
        .interact()?;

    let proxy_url: Option<String> = if want_proxy {
        let url: String = Input::new()
            .with_prompt("Upstream URL")
            .default("http://localhost:4000".to_owned())
            .interact_text()?;
        Some(url)
    } else {
        None
    };

    // ── TLS ──────────────────────────────────────────────────────────────────
    let tls_config = ask_tls_config()?;

    // ── Health check ─────────────────────────────────────────────────────────
    let want_health = Confirm::new()
        .with_prompt("Enable health check endpoint (GET /__health__)?")
        .default(true)
        .interact()?;

    // ── Logging ──────────────────────────────────────────────────────────────
    let log_formats = [
        "dev (human-readable)",
        "json (structured)",
        "combined (Apache)",
        "none",
    ];
    let log_choice = Select::new()
        .with_prompt("Log format")
        .items(log_formats)
        .default(0)
        .interact()?;

    let logging_value: Option<Value> = match log_choice {
        0 => Some(json!("dev")),
        1 => Some(json!("json")),
        2 => Some(json!("combined")),
        _ => None,
    };

    // ── Assemble config ──────────────────────────────────────────────────────
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

    // ── Serialize ────────────────────────────────────────────────────────────
    let config_str = match format {
        Format::Yaml => to_yaml_string(&site)?,
        Format::Json => serde_json::to_string_pretty(&site)?,
    };

    std::fs::write(path, &config_str)?;

    println!("\nWrote {resolved_path}:\n");
    println!("{config_str}");
    println!("Run `conduit -c {resolved_path}` to start, or `conduit validate -c {resolved_path}` to check the config.");

    Ok(())
}
