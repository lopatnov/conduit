use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "conduit", about = "High-performance reverse proxy", version)]
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

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Validate config file and exit (0 = OK)
    Validate(ValidateArgs),
    /// Format config file to stdout (or overwrite with --write)
    Fmt(FmtArgs),
    /// Create conduit.json interactively
    Init(InitArgs),
    /// HEAD each upstream and report latency
    Probe(ProbeArgs),
    /// Reload config via Admin API
    Reload(AdminArgs),
    /// Show server status
    Status(AdminArgs),
    /// Graceful shutdown via Admin API
    Shutdown(AdminArgs),
    /// Manage upstream targets at runtime
    Upstreams(UpstreamsArgs),
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
pub struct InitArgs {}

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
}

#[derive(Args)]
pub struct UpstreamRemoveArgs {
    #[arg(long)]
    pub route: String,
    #[arg(long)]
    pub target: String,
}

#[derive(Args)]
pub struct UpstreamWeightArgs {
    #[arg(long)]
    pub route: String,
    #[arg(long)]
    pub target: String,
    #[arg(long)]
    pub weight: u32,
}
