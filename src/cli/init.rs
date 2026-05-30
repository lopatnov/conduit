use std::path::Path;

use dialoguer::{Confirm, Input, Select};
use serde_json::{json, Value};

/// Ask the user how to configure TLS (if at all).
///
/// Returns `None` when the user opts out of TLS, or a JSON value representing
/// either a manual-cert or ACME block.
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

/// Run the interactive `conduit init` wizard.
///
/// Asks a series of questions and writes a `conduit.json` to `output_path`.
/// Returns `Ok(())` on success or an error string on failure.
pub fn run_init(output_path: &str) -> anyhow::Result<()> {
    let path = Path::new(output_path);
    if path.exists() {
        let overwrite = Confirm::new()
            .with_prompt(format!("{output_path} already exists. Overwrite?"))
            .default(false)
            .interact()?;
        if !overwrite {
            println!("Aborted.");
            return Ok(());
        }
    }

    println!("Welcome to conduit init! Answer a few questions to get started.\n");

    // ── Port ────────────────────────────────────────────────────────────────
    let port: u16 = Input::new()
        .with_prompt("Port")
        .default(8080u16)
        .interact_text()?;

    // ── Static file serving ─────────────────────────────────────────────────
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

    // ── Reverse proxy ───────────────────────────────────────────────────────
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

    // ── TLS ─────────────────────────────────────────────────────────────────
    let tls_config = ask_tls_config()?;

    // ── Health check ────────────────────────────────────────────────────────
    let want_health = Confirm::new()
        .with_prompt("Enable health check endpoint (GET /__health__)?")
        .default(true)
        .interact()?;

    // ── Logging ─────────────────────────────────────────────────────────────
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
        _ => None, // "none" → no logging field
    };

    // ── Assemble config ─────────────────────────────────────────────────────
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

    let config_str = serde_json::to_string_pretty(&site)?;

    std::fs::write(path, &config_str)?;

    println!("\nWrote {output_path}:\n");
    println!("{config_str}");
    println!("\nRun `conduit` to start the server, or `conduit validate` to check the config.");

    Ok(())
}
