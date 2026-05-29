use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process;
use std::time::{Duration, Instant};

use clap::{CommandFactory, Parser};
use clap_complete::Shell as ClapShell;
use conduit::cli::args::{Cli, Command, Shell, UpstreamsCommand};
use conduit::cli::init;
use conduit::config::schema::{AppConfig, ProxyConfig, ProxyRouteTarget, ProxyTarget};
use conduit::config::{self, validate};
use conduit::server::builder;
use indicatif::{ProgressBar, ProgressStyle};

fn main() {
    // Initialise tracing with an env-filter so that RUST_LOG controls output.
    // Defaults to "warn" when RUST_LOG is unset; set RUST_LOG=conduit=info
    // (or =debug) for verbose output.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        None => run_server(&cli.config),
        Some(Command::Validate(_)) => cmd_validate(&cli.config),
        Some(Command::Fmt(args)) => cmd_fmt(&cli.config, args.write),
        Some(Command::Init(args)) => {
            let output = args.output.as_deref().unwrap_or(&cli.config);
            if let Err(e) = init::run_init(output) {
                eprintln!("error: {e}");
                process::exit(1);
            }
        }
        Some(Command::Probe(_)) => cmd_probe(&cli.config),
        Some(Command::Reload(args)) => {
            let addr = resolve_admin(args.admin.as_deref());
            admin_post("reload", &addr);
        }
        Some(Command::Status(args)) => {
            let addr = resolve_admin(args.admin.as_deref());
            admin_get("status", &addr);
        }
        Some(Command::Shutdown(args)) => {
            let addr = resolve_admin(args.admin.as_deref());
            admin_post("shutdown", &addr);
        }
        Some(Command::Completions(args)) => {
            let clap_shell = match args.shell {
                Shell::Bash => ClapShell::Bash,
                Shell::Zsh => ClapShell::Zsh,
                Shell::Fish => ClapShell::Fish,
                Shell::PowerShell => ClapShell::PowerShell,
                Shell::Elvish => ClapShell::Elvish,
            };
            clap_complete::generate(
                clap_shell,
                &mut Cli::command(),
                "conduit",
                &mut std::io::stdout(),
            );
        }
        Some(Command::Man) => {
            let cmd = Cli::command();
            let man = clap_mangen::Man::new(cmd);
            man.render(&mut std::io::stdout()).unwrap_or_else(|e| {
                eprintln!("error generating man page: {e}");
                process::exit(1);
            });
        }
        Some(Command::Upstreams(args)) => {
            let addr = resolve_admin(args.admin.as_deref());
            match args.command {
                None => admin_get("upstreams", &addr),
                Some(UpstreamsCommand::Add(a)) => {
                    let weight = a.weight.unwrap_or(1);
                    let mut obj = serde_json::json!({
                        "route":  a.route,
                        "target": a.target,
                        "weight": weight,
                    });
                    if let Some(site) = &a.site {
                        obj["site"] = serde_json::json!(site);
                    }
                    admin_post_json("upstreams/add", &addr, &obj.to_string());
                }
                Some(UpstreamsCommand::Remove(r)) => {
                    let mut obj = serde_json::json!({
                        "route":  r.route,
                        "target": r.target,
                    });
                    if let Some(site) = &r.site {
                        obj["site"] = serde_json::json!(site);
                    }
                    admin_post_json("upstreams/remove", &addr, &obj.to_string());
                }
                Some(UpstreamsCommand::Weight(w)) => {
                    let mut obj = serde_json::json!({
                        "route":  w.route,
                        "target": w.target,
                        "weight": w.weight,
                    });
                    if let Some(site) = &w.site {
                        obj["site"] = serde_json::json!(site);
                    }
                    admin_post_json("upstreams/weight", &addr, &obj.to_string());
                }
            }
        }
    }
}

// ── Server ─────────────────────────────────────────────────────────────────

fn run_server(config_path: &str) {
    let path = Path::new(config_path);
    let cfg = match config::load_config(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error loading config: {e}");
            process::exit(1);
        }
    };
    let errors = validate::validate(&cfg);
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("config error at {}: {}", e.path, e.message);
        }
        process::exit(1);
    }
    if let Err(e) = builder::run_server(cfg, path.to_path_buf()) {
        eprintln!("server error: {e}");
        process::exit(1);
    }
}

// ── validate ───────────────────────────────────────────────────────────────

fn cmd_validate(config_path: &str) {
    let path = Path::new(config_path);
    let app = match config::load_config(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
    let errors = validate::validate(&app);
    if errors.is_empty() {
        let site_count = app.sites.len();
        let route_count: usize = app
            .sites
            .iter()
            .map(|s| {
                use crate::config::schema::ProxyConfig;
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

// ── fmt ────────────────────────────────────────────────────────────────────

fn cmd_fmt(config_path: &str, write: bool) {
    let path = Path::new(config_path);
    let app = match config::load_config(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
    let formatted = match serde_json::to_string_pretty(&app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error serializing config: {e}");
            process::exit(1);
        }
    };
    if write {
        if let Err(e) = std::fs::write(path, &formatted) {
            eprintln!("error writing {}: {e}", path.display());
            process::exit(1);
        }
        println!("Formatted {} in place.", path.display());
    } else {
        println!("{formatted}");
    }
}

// ── probe ──────────────────────────────────────────────────────────────────

fn cmd_probe(config_path: &str) {
    let path = Path::new(config_path);
    let app = match config::load_config(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error loading config: {e}");
            process::exit(1);
        }
    };

    let urls = collect_upstream_urls(&app);
    if urls.is_empty() {
        println!("No upstream URLs found in config.");
        return;
    }

    println!("Probing {} upstream(s)...\n", urls.len());

    let pb = ProgressBar::new(urls.len() as u64);
    pb.set_style(
        ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );

    let mut results: Vec<(String, String, Option<u16>, Duration)> = Vec::new();

    for url in &urls {
        pb.set_message(url.clone());
        let (status_str, status_code, elapsed) = probe_url(url);
        results.push((url.clone(), status_str, status_code, elapsed));
        pb.inc(1);
    }
    pb.finish_and_clear();

    // Print aligned results table.
    let url_width = results.iter().map(|(u, ..)| u.len()).max().unwrap_or(10);
    let status_width = results.iter().map(|(_, s, ..)| s.len()).max().unwrap_or(10);

    println!(
        "{:<url_width$}  {:<status_width$}  Latency",
        "URL", "Status"
    );
    println!("{}", "-".repeat(url_width + status_width + 12));

    let mut any_error = false;
    for (url, status_str, status_code, elapsed) in &results {
        let ms = elapsed.as_millis();
        println!("{url:<url_width$}  {status_str:<status_width$}  {ms} ms");
        if status_code.is_none() || status_code.is_some_and(|s| s >= 500) {
            any_error = true;
        }
    }

    if any_error {
        process::exit(1);
    }
}

/// Collect all unique upstream URLs from an `AppConfig`.
fn collect_upstream_urls(app: &AppConfig) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut urls = Vec::new();
    for site in &app.sites {
        for url in site_upstream_urls(site) {
            if seen.insert(url.clone()) {
                urls.push(url);
            }
        }
    }
    urls
}

/// Return every upstream URL referenced by a single site's `proxy` and `routes`.
fn site_upstream_urls(site: &crate::config::schema::SiteConfig) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(proxy) = &site.proxy {
        out.extend(extract_proxy_urls(proxy));
    }
    if let Some(routes) = &site.routes {
        for route in routes {
            if let Some(proxy_target) = &route.proxy {
                out.extend(extract_route_target_urls(proxy_target));
            }
        }
    }
    out
}

/// Flatten a `ProxyConfig` into a list of raw URL strings.
fn extract_proxy_urls(proxy: &ProxyConfig) -> Vec<String> {
    match proxy {
        ProxyConfig::Single(url) => vec![url.clone()],
        ProxyConfig::Routes(routes) => routes
            .values()
            .flat_map(extract_route_target_urls)
            .collect(),
    }
}

/// Flatten a `ProxyRouteTarget` into raw URL strings.
fn extract_route_target_urls(target: &ProxyRouteTarget) -> Vec<String> {
    match target {
        ProxyRouteTarget::Url(url) => vec![url.clone()],
        ProxyRouteTarget::RoundRobin(urls) => urls.clone(),
        ProxyRouteTarget::Full(cfg) => collect_full_target_urls(cfg),
    }
}

/// Collect every URL from a `Full` proxy route config (targets + group targets).
fn collect_full_target_urls(cfg: &crate::config::schema::ProxyRouteConfig) -> Vec<String> {
    let mut out: Vec<String> = cfg.targets.iter().map(proxy_target_url).collect();
    if let Some(groups) = &cfg.groups {
        for group in groups {
            out.extend(group.targets.iter().map(proxy_target_url));
        }
    }
    out
}

/// Extract the URL string from either form of `ProxyTarget`.
fn proxy_target_url(t: &ProxyTarget) -> String {
    match t {
        ProxyTarget::Simple(url) => url.clone(),
        ProxyTarget::Weighted(w) => w.url.clone(),
    }
}

/// Send an HTTP HEAD request (plain TCP; TLS upstreams get a TCP connectivity check only).
///
/// Returns `(status_description, status_code, elapsed)`.
fn probe_url(url: &str) -> (String, Option<u16>, Duration) {
    let start = Instant::now();

    let Some((is_tls, host, port, path)) = parse_upstream_url(url) else {
        return ("invalid URL".to_owned(), None, Duration::ZERO);
    };

    if is_tls {
        return probe_tls_tcp(&host, port, start);
    }

    // Plain HTTP: send HEAD and read the status line.
    let result = probe_http_head(&host, port, &path);
    let elapsed = start.elapsed();
    match result {
        Ok(status) => (format!("HTTP {status}"), Some(status), elapsed),
        Err(e) => (format!("error: {e}"), None, elapsed),
    }
}

/// Probe TLS upstream via TCP-only connectivity check (no TLS handshake in CLI).
///
/// Tries direct `SocketAddr` parse first, then falls back to DNS resolution.
fn probe_tls_tcp(host: &str, port: u16, start: Instant) -> (String, Option<u16>, Duration) {
    // Use the (host, port) overload so IPv6 literals like "::1" are handled
    // correctly — formatting as "{host}:{port}" produces "::1:4000" which
    // to_socket_addrs / SocketAddr::parse cannot parse.
    let addr_display = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let sock_addr = (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next());

    match sock_addr {
        None => (
            format!("unreachable: cannot resolve {addr_display}"),
            None,
            start.elapsed(),
        ),
        Some(addr) => match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(_) => (
                "TCP open (HTTPS — HEAD skipped)".to_owned(),
                None,
                start.elapsed(),
            ),
            Err(e) => (format!("unreachable: {e}"), None, start.elapsed()),
        },
    }
}

/// Send an HTTP/1.1 HEAD request over a plain TCP stream and return the status code.
fn probe_http_head(host: &str, port: u16, path: &str) -> anyhow::Result<u16> {
    let addr_display = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let sock_addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve {addr_display}"))?;
    let mut stream = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(10))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "HEAD {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    // Status line: "HTTP/1.x NNN Reason"
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("no status code in response"))
}

/// Parse an upstream URL into `(is_tls, host, port, path)`.
fn parse_upstream_url(url: &str) -> Option<(bool, String, u16, String)> {
    let url = url.trim();
    let (is_tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return None;
    };

    let (authority, path) = rest
        .find('/')
        .map(|i| (&rest[..i], rest[i..].to_owned()))
        .unwrap_or((rest, "/".to_owned()));

    // Handle IPv6 literals like [::1]:8080.
    let (host, port) = if authority.starts_with('[') {
        let bracket_end = authority.find(']').unwrap_or(authority.len());
        let host = authority[1..bracket_end].to_owned();
        let port = authority
            .get(bracket_end + 1..)
            .and_then(|s| s.strip_prefix(':'))
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(if is_tls { 443 } else { 80 });
        (host, port)
    } else if let Some(colon) = authority.rfind(':') {
        let port = authority[colon + 1..]
            .parse::<u16>()
            .unwrap_or(if is_tls { 443 } else { 80 });
        (authority[..colon].to_owned(), port)
    } else {
        (authority.to_owned(), if is_tls { 443 } else { 80 })
    };

    Some((is_tls, host, port, path))
}

// ── Admin API helpers ──────────────────────────────────────────────────────

fn resolve_admin(flag: Option<&str>) -> String {
    flag.map(ToOwned::to_owned)
        .or_else(|| std::env::var("CONDUIT_ADMIN").ok())
        .unwrap_or_else(|| "127.0.0.1:2019".to_owned())
}

fn admin_get(path: &str, addr: &str) {
    match http_get(path, addr) {
        Ok(body) => println!("{body}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

fn admin_post(path: &str, addr: &str) {
    match http_post(path, addr) {
        Ok(body) => println!("{body}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

fn http_get(path: &str, addr: &str) -> anyhow::Result<String> {
    let sock = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve {addr}"))?;
    let mut stream = TcpStream::connect_timeout(&sock, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET /{path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(extract_body(&response))
}

fn http_post(path: &str, addr: &str) -> anyhow::Result<String> {
    let sock = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve {addr}"))?;
    let mut stream = TcpStream::connect_timeout(&sock, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "POST /{path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(extract_body(&response))
}

/// POST to `path` on the admin API with a JSON body.
fn admin_post_json(path: &str, addr: &str, json_body: &str) {
    match http_post_json(path, addr, json_body) {
        Ok(body) => println!("{body}"),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

fn http_post_json(path: &str, addr: &str, json_body: &str) -> anyhow::Result<String> {
    let sock = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve {addr}"))?;
    let mut stream = TcpStream::connect_timeout(&sock, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "POST /{path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{json_body}",
        len = json_body.len()
    )?;
    stream.flush()?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(extract_body(&response))
}

fn extract_body(response: &str) -> String {
    response
        .find("\r\n\r\n")
        .map(|pos| response[pos + 4..].to_owned())
        .unwrap_or_else(|| response.to_owned())
}
