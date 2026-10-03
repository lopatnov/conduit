use clap::Parser;
use conduit::cli::args::Cli;
use conduit::cli::dispatch::dispatch_command;

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

    dispatch_command(Cli::parse());
}
