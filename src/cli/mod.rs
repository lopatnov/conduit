pub mod admin_client;
pub mod args;
pub mod config_path;
pub mod fmt;
pub mod init;
pub mod probe;
pub mod serve;
pub mod status;
pub mod upstream_urls;
pub mod validate;

/// `conduit::cli::*` is binary-support API for the `conduit` executable
/// (`src/main.rs`) — not general-purpose library API. Its functions call
/// `std::process::exit` on fatal errors and are not meant to be used from
/// other applications embedding this crate.
///
/// A CLI subcommand that can be executed.
///
/// ## Adding a new command
///
/// 1. Add a variant to `Command` in `cli/args.rs`.
/// 2. Put the command's body in `src/cli/<command>.rs` (e.g. `cli/serve.rs`)
///    as a `pub fn run(...)`, and keep a struct in `main.rs` that holds the
///    pre-extracted arguments for that command.
/// 3. `impl CliCommand for YourCmd { fn execute(self) { <module>::run(...) } }`
///    — the struct's `execute()` should stay a one-line delegating call.
/// 4. Add one arm to `dispatch_command()` in `main.rs`.
///
/// No other changes to `main()` are required.
pub trait CliCommand {
    /// Run the command.  Implementations may call `std::process::exit` on
    /// fatal errors (consistent with the binary entry-point convention).
    fn execute(self);
}
