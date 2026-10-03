use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "conduit",
    about = "High-performance reverse proxy and static file server",
    long_about = "High-performance reverse proxy and static file server built on Cloudflare Pingora.\n\
                  \n\
                  Created by Oleksandr Lopatnov\n\
                  \u{2022} GitHub:   https://github.com/lopatnov\n\
                  \u{2022} LinkedIn: https://linkedin.com/in/lopatnov",
    version
)]
pub struct Cli {
    /// Config file path
    #[arg(
        short,
        long,
        value_name = "FILE",
        default_value = "conduit.json",
        global = true
    )]
    pub config: String,

    /// Watch Kubernetes ConduitSite CRDs in this namespace instead of reading
    /// a config file. Use "*" to watch all namespaces.
    /// Requires: cargo build --features kubernetes
    #[cfg(feature = "kubernetes")]
    #[arg(long, value_name = "NAMESPACE")]
    pub kubernetes_namespace: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Validate config file and exit (0 = OK)
    Validate(ValidateArgs),
    /// Format config file to stdout (or overwrite with --write)
    Fmt(FmtArgs),
    /// Generate a starter config (interactive wizard or -y for non-interactive)
    Init(InitArgs),
    /// HEAD each upstream and report latency
    Probe(ProbeArgs),
    /// Reload config via Admin API
    Reload(AdminArgs),
    /// Show server status and optionally upstream health
    Status(StatusArgs),
    /// Graceful shutdown via Admin API
    Shutdown(AdminArgs),
    /// Manage upstream targets at runtime
    Upstreams(UpstreamsArgs),
    /// Print shell completions to stdout
    Completions(CompletionsArgs),
    /// Generate man page to stdout
    Man,
}

#[derive(Args)]
pub struct ValidateArgs {}

#[derive(Args)]
pub struct FmtArgs {
    /// Overwrite the config file instead of printing to stdout
    #[arg(long)]
    pub write: bool,
}

#[derive(Args)]
pub struct InitArgs {
    /// Write config to this file instead of the default config path
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<String>,

    /// Non-interactive: accept all defaults without prompting.
    /// Individual flags still override the defaults.
    #[arg(short, long)]
    pub yes: bool,

    /// Output format: "yaml" (default) or "json".
    /// Inferred from -o extension when provided.
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<String>,

    /// Port to listen on [default: 8080]
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,

    /// Serve static files from this directory [default: ./dist]
    #[arg(long, value_name = "DIR")]
    pub static_dir: Option<String>,

    /// Disable static file serving (overrides --static-dir)
    #[arg(long)]
    pub no_static: bool,

    /// Proxy requests to this upstream URL (e.g. http://localhost:4000)
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,

    /// Disable proxy (overrides --proxy)
    #[arg(long)]
    pub no_proxy: bool,

    /// Log format: "dev", "json", "combined", or "none" [default: dev]
    #[arg(long, value_name = "FORMAT")]
    pub log: Option<String>,

    /// Disable the /__health__ endpoint
    #[arg(long)]
    pub no_health: bool,

    /// TLS certificate file (enables manual-cert TLS)
    #[arg(long, value_name = "FILE", requires = "tls_key")]
    pub tls_cert: Option<String>,

    /// TLS private key file (required with --tls-cert)
    #[arg(long, value_name = "FILE", requires = "tls_cert")]
    pub tls_key: Option<String>,

    /// ACME / Let's Encrypt email (enables auto-TLS)
    #[arg(long, value_name = "EMAIL")]
    pub tls_acme: Option<String>,
}

#[derive(Args)]
pub struct StatusArgs {
    /// Admin API address
    #[arg(long, value_name = "ADDR", env = "CONDUIT_ADMIN")]
    pub admin: Option<String>,
    /// Show upstream health table (URL, healthy, latency, ejected, 5xx count)
    #[arg(long)]
    pub upstream: bool,
}

#[derive(Args)]
pub struct ProbeArgs {}

#[derive(Args)]
pub struct AdminArgs {
    /// Admin API address
    #[arg(long, value_name = "ADDR", env = "CONDUIT_ADMIN")]
    pub admin: Option<String>,
}

#[derive(Args)]
pub struct UpstreamsArgs {
    /// Admin API address
    #[arg(long, value_name = "ADDR", env = "CONDUIT_ADMIN", global = true)]
    pub admin: Option<String>,

    #[command(subcommand)]
    pub command: Option<UpstreamsCommand>,
}

#[derive(Subcommand)]
pub enum UpstreamsCommand {
    /// Add a target to a route (in-memory only; lost on restart)
    Add(UpstreamAddArgs),
    /// Remove a target from a route (in-memory only)
    Remove(UpstreamRemoveArgs),
    /// Change a target's weight (WeightedRoundRobin only)
    Weight(UpstreamWeightArgs),
}

/// Shell for which to generate completions.
#[derive(ValueEnum, Clone, Debug)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
}

#[derive(Args)]
pub struct CompletionsArgs {
    /// Target shell
    pub shell: Shell,
}

#[derive(Args)]
pub struct UpstreamAddArgs {
    /// Proxy route path (e.g. /api)
    #[arg(long)]
    pub route: String,
    /// Upstream URL (e.g. http://backend:4000)
    #[arg(long)]
    pub target: String,
    /// Weight (for weighted-round-robin)
    #[arg(long)]
    pub weight: Option<u32>,
    /// Limit the override to a specific site label (e.g. app.example.com:443).
    /// When omitted the override applies to every site that serves this route.
    #[arg(long)]
    pub site: Option<String>,
}

#[derive(Args)]
pub struct UpstreamRemoveArgs {
    #[arg(long)]
    pub route: String,
    #[arg(long)]
    pub target: String,
    /// Limit the removal to a specific site label.
    /// When omitted the wildcard override (all sites) is targeted.
    #[arg(long)]
    pub site: Option<String>,
}

#[derive(Args)]
pub struct UpstreamWeightArgs {
    #[arg(long)]
    pub route: String,
    #[arg(long)]
    pub target: String,
    #[arg(long)]
    pub weight: u32,
    /// Limit the weight change to a specific site label.
    /// When omitted the wildcard override (all sites) is targeted.
    #[arg(long)]
    pub site: Option<String>,
}
