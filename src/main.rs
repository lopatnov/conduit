use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process;
use std::time::{Duration, Instant};

use clap::{CommandFactory, Parser};
use clap_complete::Shell as ClapShell;
use conduit::cli::args::{Cli, Command, Shell, UpstreamsCommand};
use conduit::cli::init;
use conduit::cli::CliCommand;
use conduit::config::schema::{AppConfig, ProxyConfig, ProxyRouteTarget, ProxyTarget};
use conduit::config::{self, validate};
use conduit::server::builder;

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
    dispatch_command(cli);
}

/// Build the right [`CliCommand`] implementation from the parsed CLI and run it.
///
/// Adding a new command: add one arm here only — `main()` does not change.
fn dispatch_command(cli: Cli) {
    let config = resolve_config_path(&cli.config);

    // Kubernetes CRD mode: when --kubernetes-namespace is set and no subcommand
    // is given, start the server using the KubernetesProvider instead of a file.
    #[cfg(feature = "kubernetes")]
    if let (Some(ns), None) = (&cli.kubernetes_namespace, &cli.command) {
        run_server_kubernetes(ns);
        return;
    }

    match cli.command {
        None => ServeCmd {
            config_path: config,
        }
        .execute(),
        Some(Command::Validate(_)) => ValidateCmd {
            config_path: config,
        }
        .execute(),
        Some(Command::Fmt(args)) => FmtCmd {
            config_path: config,
            write: args.write,
        }
        .execute(),
        Some(Command::Init(args)) => {
            InitCmd {
                output: args.output,
                yes: args.yes,
                format: args.format,
                port: args.port,
                static_dir: args.static_dir,
                no_static: args.no_static,
                proxy: args.proxy,
                no_proxy: args.no_proxy,
                log: args.log,
                no_health: args.no_health,
                tls_cert: args.tls_cert,
                tls_key: args.tls_key,
                tls_acme: args.tls_acme,
            }
            .execute();
        }
        Some(Command::Probe(_)) => ProbeCmd {
            config_path: config,
        }
        .execute(),
        Some(Command::Reload(args)) => {
            ReloadCmd {
                admin_addr: resolve_admin(args.admin.as_deref()),
            }
            .execute();
        }
        Some(Command::Status(args)) => {
            StatusCmd {
                admin_addr: resolve_admin(args.admin.as_deref()),
                upstream: args.upstream,
            }
            .execute();
        }
        Some(Command::Shutdown(args)) => {
            ShutdownCmd {
                admin_addr: resolve_admin(args.admin.as_deref()),
            }
            .execute();
        }
        Some(Command::Completions(args)) => CompletionsCmd { shell: args.shell }.execute(),
        Some(Command::Man) => ManCmd.execute(),
        Some(Command::Upstreams(args)) => {
            UpstreamsCmd {
                admin_addr: resolve_admin(args.admin.as_deref()),
                subcommand: args.command,
            }
            .execute();
        }
    }
}

// ── Command structs ────────────────────────────────────────────────────────────

struct ServeCmd {
    config_path: String,
}
impl CliCommand for ServeCmd {
    fn execute(self) {
        run_server(&self.config_path);
    }
}

struct ValidateCmd {
    config_path: String,
}
impl CliCommand for ValidateCmd {
    fn execute(self) {
        cmd_validate(&self.config_path);
    }
}

struct FmtCmd {
    config_path: String,
    write: bool,
}
impl CliCommand for FmtCmd {
    fn execute(self) {
        cmd_fmt(&self.config_path, self.write);
    }
}

struct InitCmd {
    output: Option<String>,
    yes: bool,
    format: Option<String>,
    port: Option<u16>,
    static_dir: Option<String>,
    no_static: bool,
    proxy: Option<String>,
    no_proxy: bool,
    log: Option<String>,
    no_health: bool,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    tls_acme: Option<String>,
}
impl CliCommand for InitCmd {
    fn execute(self) {
        let opts = init::InitOptions {
            output: self.output.as_deref(),
            yes: self.yes,
            format: self.format.as_deref(),
            port: self.port,
            static_dir: self.static_dir.as_deref(),
            no_static: self.no_static,
            proxy: self.proxy.as_deref(),
            no_proxy: self.no_proxy,
            log: self.log.as_deref(),
            no_health: self.no_health,
            tls_cert: self.tls_cert.as_deref(),
            tls_key: self.tls_key.as_deref(),
            tls_acme: self.tls_acme.as_deref(),
        };
        if let Err(e) = init::run_init(opts) {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

struct ProbeCmd {
    config_path: String,
}
impl CliCommand for ProbeCmd {
    fn execute(self) {
        cmd_probe(&self.config_path);
    }
}

struct ReloadCmd {
    admin_addr: String,
}
impl CliCommand for ReloadCmd {
    fn execute(self) {
        admin_post("reload", &self.admin_addr);
    }
}

struct StatusCmd {
    admin_addr: String,
    upstream: bool,
}
impl CliCommand for StatusCmd {
    fn execute(self) {
        if self.upstream {
            print_upstream_table(&self.admin_addr);
        } else {
            admin_get("status", &self.admin_addr);
        }
    }
}

struct ShutdownCmd {
    admin_addr: String,
}
impl CliCommand for ShutdownCmd {
    fn execute(self) {
        admin_post("shutdown", &self.admin_addr);
    }
}

struct CompletionsCmd {
    shell: Shell,
}
impl CliCommand for CompletionsCmd {
    fn execute(self) {
        let clap_shell = match self.shell {
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
}

struct ManCmd;
impl CliCommand for ManCmd {
    fn execute(self) {
        let cmd = Cli::command();
        let man = clap_mangen::Man::new(cmd);
        man.render(&mut std::io::stdout()).unwrap_or_else(|e| {
            eprintln!("error generating man page: {e}");
            process::exit(1);
        });
    }
}

struct UpstreamsCmd {
    admin_addr: String,
    subcommand: Option<UpstreamsCommand>,
}
impl CliCommand for UpstreamsCmd {
    fn execute(self) {
        match self.subcommand {
            None => admin_get("upstreams", &self.admin_addr),
            Some(UpstreamsCommand::Add(a)) => {
                let body = upstream_json(
                    &a.route,
                    &a.target,
                    Some(a.weight.unwrap_or(1)),
                    a.site.as_deref(),
                );
                admin_post_json("upstreams/add", &self.admin_addr, &body);
            }
            Some(UpstreamsCommand::Remove(r)) => {
                let body = upstream_json(&r.route, &r.target, None, r.site.as_deref());
                admin_post_json("upstreams/remove", &self.admin_addr, &body);
            }
            Some(UpstreamsCommand::Weight(w)) => {
                let body = upstream_json(&w.route, &w.target, Some(w.weight), w.site.as_deref());
                admin_post_json("upstreams/weight", &self.admin_addr, &body);
            }
        }
    }
}

// ── Upstream command helpers ───────────────────────────────────────────────

/// Build the JSON body for `/upstreams/add`, `/remove`, and `/weight`.
///
/// Extracted to keep `main()` cognitive complexity within the allowed limit.
fn upstream_json(route: &str, target: &str, weight: Option<u32>, site: Option<&str>) -> String {
    let mut obj = serde_json::json!({ "route": route, "target": target });
    if let Some(w) = weight {
        obj["weight"] = serde_json::json!(w);
    }
    if let Some(s) = site {
        obj["site"] = serde_json::json!(s);
    }
    obj.to_string()
}

// ── Config path resolution ─────────────────────────────────────────────────

/// Resolve the config path, trying YAML alternatives when the default JSON
/// path does not exist.
///
/// Priority: explicit path → conduit.json → conduit.yaml → conduit.yml.
/// Only applies when the user did not pass `-c` explicitly (i.e. the value
/// is still the default "conduit.json" and that file is absent).
fn resolve_config_path(config_arg: &str) -> String {
    let path = Path::new(config_arg);
    if path.exists() {
        return config_arg.to_owned();
    }
    // Only auto-probe alternatives when the argument is the default value.
    if config_arg == "conduit.json" {
        for alt in &["conduit.yaml", "conduit.yml"] {
            if Path::new(alt).exists() {
                return (*alt).to_owned();
            }
        }
    }
    config_arg.to_owned()
}

// ── Server ─────────────────────────────────────────────────────────────────

fn run_server(config_path: &str) {
    let path = Path::new(&config_path);
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
    for w in validate::feature_warnings(&cfg) {
        tracing::warn!("feature not compiled in: {w}");
    }
    if let Err(e) = builder::run_server(cfg, path.to_path_buf(), None) {
        eprintln!("server error: {e}");
        process::exit(1);
    }
}

// ── Kubernetes provider startup ────────────────────────────────────────────

/// Start Conduit using Kubernetes `ConduitSite` CRDs as the config source.
///
/// Spawns a background thread that runs the [`KubernetesProvider`], waits for
/// the initial config, then starts the server. Subsequent CRD changes are
/// received by the server's live-update watcher and hot-swapped without restart.
///
/// Requires: `cargo build --features kubernetes`.
#[cfg(feature = "kubernetes")]
fn run_server_kubernetes(namespace: &str) {
    use conduit::config::kubernetes::KubernetesProvider;
    use conduit::config::provider::Provider;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<conduit::config::schema::AppConfig>(4);
    let ns = namespace.to_owned();

    // Spawn the provider in its own thread with a dedicated Tokio runtime.
    std::thread::Builder::new()
        .name("kubernetes-provider".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime for kubernetes-provider");
            if let Err(e) = rt.block_on(KubernetesProvider::new(ns).run(tx)) {
                eprintln!("kubernetes provider error: {e}");
                process::exit(1);
            }
        })
        .expect("failed to spawn kubernetes-provider thread");

    // Block until the provider delivers the initial config (or the channel closes).
    let initial_config = match rx.blocking_recv() {
        Some(cfg) => cfg,
        None => {
            eprintln!("error: kubernetes provider closed before sending initial config");
            process::exit(1);
        }
    };

    tracing::info!(
        namespace,
        sites = initial_config.sites.len(),
        "initial config loaded from ConduitSite CRDs"
    );

    // Start the server; pass `rx` so CRD changes are hot-swapped automatically.
    if let Err(e) = builder::run_server(initial_config, std::path::PathBuf::new(), Some(rx)) {
        eprintln!("server error: {e}");
        process::exit(1);
    }
}

// ── validate ───────────────────────────────────────────────────────────────

fn cmd_validate(config_path: &str) {
    let path = Path::new(&config_path);
    let app = match config::load_config(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
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
    let path = Path::new(&config_path);
    let app = match config::load_config(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    // Preserve the input format: YAML files stay YAML, JSON files stay JSON.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let is_yaml = ext == "yaml" || ext == "yml";

    let formatted = if is_yaml {
        match serde_yaml::to_string(&app) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error serializing config: {e}");
                process::exit(1);
            }
        }
    } else {
        match serde_json::to_string_pretty(&app) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error serializing config: {e}");
                process::exit(1);
            }
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

// ── conduit status --upstream ──────────────────────────────────────────────

/// Fetch upstream health from the Admin API and print a formatted table.
fn print_upstream_table(addr: &str) {
    let body = match http_get("upstreams", addr) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error parsing response: {e}");
            process::exit(1);
        }
    };
    let flat = match json.get("upstreams").and_then(|v| v.as_array()) {
        Some(arr) => arr.clone(),
        None => {
            // Fallback: the whole response might be an array.
            match json.as_array() {
                Some(arr) => arr.clone(),
                None => {
                    println!("{body}");
                    return;
                }
            }
        }
    };

    if flat.is_empty() {
        println!("No upstreams registered.");
        return;
    }

    // Compute column widths.
    let url_w = flat
        .iter()
        .filter_map(|e| e.get("url").and_then(|v| v.as_str()))
        .map(|s| s.len())
        .max()
        .unwrap_or(3)
        .max(3);

    println!(
        "{:<url_w$}  {:<7}  {:<10}  {:<7}  5xx",
        "URL", "Healthy", "Latency", "Ejected"
    );
    println!("{}", "─".repeat(url_w + 42)); // println! used to avoid format! overhead

    for entry in &flat {
        let url = entry
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let healthy = entry
            .get("healthy")
            .and_then(|v| v.as_bool())
            .map(|b| if b { "✓" } else { "✗" })
            .unwrap_or("?");
        let latency = entry
            .get("latency_ms")
            .and_then(|v| v.as_u64())
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_else(|| "—".to_owned());
        let ejected = entry
            .get("ejected")
            .and_then(|v| v.as_bool())
            .map(|b| if b { "yes" } else { "no" })
            .unwrap_or("—");
        let consec_5xx = entry
            .get("consecutive_5xx")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "0".to_owned());
        println!(
            "{:<url_w$}  {:<7}  {:<10}  {:<7}  {}",
            url, healthy, latency, ejected, consec_5xx
        );
    }
    println!();
    let healthy_count = flat
        .iter()
        .filter(|e| {
            e.get("healthy")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .count();
    println!("{}/{} upstreams healthy", healthy_count, flat.len());
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
