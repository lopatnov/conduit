use clap::Parser;
use conduit::cli::args::Cli;
use conduit::cli::dispatch::dispatch_command;

// Global allocator (issue #486): the binary only, so a library user keeps theirs. mimalloc is on by default; the two
// features are alternatives, and with both on (`--features jemalloc` on a default build, `--all-features`) jemalloc is
// used, so asking for it is never overridden by the default. Neither on: the system allocator.
#[cfg(all(feature = "mimalloc", not(feature = "jemalloc")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

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
