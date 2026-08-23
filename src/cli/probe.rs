use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process;
use std::time::{Duration, Instant};

use super::config_path::load_config_or_exit;
use super::upstream_urls::collect_upstream_urls;

// ── probe ──────────────────────────────────────────────────────────────────

pub fn run(config_path: &str) {
    let path = Path::new(config_path);
    let app = load_config_or_exit(path);

    let urls = collect_upstream_urls(&app);
    if urls.is_empty() {
        println!("No upstream URLs found in config.");
        return;
    }

    println!("Probing {} upstream(s) in parallel...\n", urls.len());

    // Probe all upstreams in parallel — each gets its own thread.
    let handles: Vec<_> = urls
        .iter()
        .map(|url| {
            let url_owned = url.clone();
            std::thread::spawn(move || {
                let (status_str, status_code, elapsed) = probe_url(&url_owned);
                (url_owned, status_str, status_code, elapsed)
            })
        })
        .collect();

    // Collect results in original order.
    let mut results: Vec<(String, String, Option<u16>, Duration)> =
        handles.into_iter().filter_map(|h| h.join().ok()).collect();

    // Sort: failures first (so they're easy to spot), then by URL.
    results.sort_by_key(|(url, _, code, _)| {
        let is_ok = code.is_some_and(|s| s < 500);
        (!is_ok, url.clone())
    });

    // Print aligned results table.
    let url_width = results
        .iter()
        .map(|(u, ..)| u.len())
        .max()
        .unwrap_or(10)
        .max(3);
    let status_width = results
        .iter()
        .map(|(_, s, ..)| s.len())
        .max()
        .unwrap_or(10)
        .max(6);

    println!(
        "{:<url_width$}  {:<status_width$}  Latency",
        "URL", "Status"
    );
    println!("{}", "─".repeat(url_width + status_width + 12));

    let mut any_error = false;
    for (url, status_str, status_code, elapsed) in &results {
        let ms = elapsed.as_millis();
        let is_ok = status_code.is_some_and(|s| s < 500);
        let indicator = if is_ok { "✓" } else { "✗" };
        println!("{indicator} {url:<url_width$}  {status_str:<status_width$}  {ms} ms");
        if !is_ok {
            any_error = true;
        }
    }

    println!();
    let ok_count = results
        .iter()
        .filter(|(.., code, _)| code.is_some_and(|s| s < 500))
        .count();
    println!("{ok_count}/{} upstreams healthy", results.len());

    if any_error {
        process::exit(1);
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
    } else {
        let rest = url.strip_prefix("http://")?;
        (false, rest)
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
