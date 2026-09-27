use std::path::Path;
use std::process;

use crate::config::schema::ProxyConfig;
use crate::config::validate;

use super::config_path::load_config_or_exit;

// ── validate ───────────────────────────────────────────────────────────────

pub fn run(config_path: &str) {
    let path = Path::new(&config_path);
    let app = load_config_or_exit(path);
    let errors = validate::validate(&app);
    let warnings = validate::feature_warnings(&app);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    if errors.is_empty() {
        let site_count = app.sites.len();
        let route_count: usize = app
            .sites
            .iter()
            .map(|s| {
                let proxy_routes = match &s.proxy {
                    Some(ProxyConfig::Single(_)) => 1,
                    Some(ProxyConfig::Routes(r)) => r.len(),
                    None => 0,
                };
                let explicit_routes = s.routes.as_ref().map(|r| r.len()).unwrap_or(0);
                proxy_routes + explicit_routes
            })
            .sum();
        if route_count > 0 {
            println!(
                "Config is valid — {site_count} site{}, {route_count} route{}.",
                if site_count == 1 { "" } else { "s" },
                if route_count == 1 { "" } else { "s" },
            );
        } else {
            println!(
                "Config is valid — {site_count} site{}.",
                if site_count == 1 { "" } else { "s" },
            );
        }
    } else {
        let error_count = errors.len();
        for e in &errors {
            eprintln!("error at {}: {}", e.path, e.message);
        }
        eprintln!(
            "\n{error_count} error{} found.",
            if error_count == 1 { "" } else { "s" }
        );
        process::exit(1);
    }
}
