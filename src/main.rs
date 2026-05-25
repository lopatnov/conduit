use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process;
use std::time::{Duration, Instant};

use clap::Parser;
use conduit::cli::args::{Cli, Command, UpstreamsCommand};
use conduit::cli::init;
use conduit::config::schema::{AppConfig, ProxyConfig, ProxyRouteTarget, ProxyTarget};
use conduit::config::{self, validate};
use conduit::server::builder;
use indicatif::{ProgressBar, ProgressStyle};

fn main() {
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
        Some(Command::Upstreams(args)) => {
            let addr = resolve_admin(args.admin.as_deref());
            match args.command {
                None => admin_get("upstreams", &addr),
                Some(UpstreamsCommand::Add(a)) => {
                    let weight = a.weight.unwrap_or(1);
                    let body = format!(
                        r#"{{"route":{},"target":{},"weight":{}}}"#,
                        serde_json::to_string(&a.route).unwrap(),
                        serde_json::to_string(&a.target).unwrap(),
                        weight
                    );
                    admin_post_json("upstreams/add", &addr, &body);
                }
                Some(UpstreamsCommand::Remove(r)) => {
                    let body = format!(
                        r#"{{"route":{},"target":{}}}"#,
                        serde_json::to_string(&r.route).unwrap(),
                        serde_json::to_string(&r.target).unwrap(),
                    );
                    admin_post_json("upstreams/remove", &addr, &body);
                }
                Some(UpstreamsCommand::Weight(w)) => {
                    let body = format!(
                        r#"{{"route":{},"target":{},"weight":{}}}"#,
                        serde_json::to_string(&w.route).unwrap(),
                        serde_json::to_string(&w.target).unwrap(),
                        w.weight
                    );
                    admin_post_json("upstreams/weight", &addr, &body);
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
    if let Err(e) = builder::run_server(cfg) {
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
        println!("Config is valid.");
    } else {
        for e in &errors {
            eprintln!("error at {}: {}", e.path, e.message);
        }
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
        let Some(proxy) = &site.proxy else { continue };
        for url in extract_proxy_urls(proxy) {
            if seen.insert(url.clone()) {
                urls.push(url);
            }
        }
    }
    urls
}

/// Flatten a `ProxyConfig` into a list of raw URL strings.
fn extract_proxy_urls(proxy: &ProxyConfig) -> Vec<String> {
    match proxy {
        ProxyConfig::Single(url) => vec![url.clone()],
        ProxyConfig::Routes(routes) => {
            let mut out = Vec::new();
            for target in routes.values() {
                match target {
                    ProxyRouteTarget::Url(url) => out.push(url.clone()),
                    ProxyRouteTarget::RoundRobin(urls) => out.extend(urls.iter().cloned()),
                    ProxyRouteTarget::Full(cfg) => {
                        for t in &cfg.targets {
                            out.push(match t {
                                ProxyTarget::Simple(url) => url.clone(),
                                ProxyTarget::Weighted(w) => w.url.clone(),
                            });
                        }
                    }
                }
            }
            out
        }
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
        // Probe TCP connectivity only — no TLS stack in the CLI binary.
        let addr_str = format!("{host}:{port}");
        match addr_str.parse::<std::net::SocketAddr>() {
            Ok(addr) => match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
                Ok(_) => {
                    return (
                        "TCP open (HTTPS — HEAD skipped)".to_owned(),
                        None,
                        start.elapsed(),
                    )
                }
                Err(e) => return (format!("unreachable: {e}"), None, start.elapsed()),
            },
            Err(_) => {
                // host:port needs DNS resolution — resolve then connect with timeout.
                let resolved = addr_str.to_socket_addrs().ok().and_then(|mut a| a.next());
                match resolved {
                    Some(sock) => match TcpStream::connect_timeout(&sock, Duration::from_secs(5)) {
                        Ok(_) => {
                            return (
                                "TCP open (HTTPS — HEAD skipped)".to_owned(),
                                None,
                                start.elapsed(),
                            )
                        }
                        Err(e) => return (format!("unreachable: {e}"), None, start.elapsed()),
                    },
                    None => {
                        return (
                            format!("unreachable: cannot resolve {addr_str}"),
                            None,
                            start.elapsed(),
                        )
                    }
                }
            }
        }
    }

    // Plain HTTP: send HEAD and read the status line.
    let result = (|| -> anyhow::Result<u16> {
        let addr_str = format!("{host}:{port}");
        let sock_addr = addr_str
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow::anyhow!("cannot resolve {addr_str}"))?;
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
        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| anyhow::anyhow!("no status code in response"))?;
        Ok(status)
    })();

    let elapsed = start.elapsed();
    match result {
        Ok(status) => (format!("HTTP {status}"), Some(status), elapsed),
        Err(e) => (format!("error: {e}"), None, elapsed),
    }
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
