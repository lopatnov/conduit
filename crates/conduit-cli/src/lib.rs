//! `main()`'s CLI subcommand dispatch for conduit's feature-driven Cargo workspace (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted in
//! [#147](https://github.com/lopatnov/conduit/issues/147)).
//!
//! `conduit_cli::*` is binary-support API for the `conduit` executable (`src/main.rs`, in the root crate) — not
//! general-purpose library API. Its functions call `std::process::exit` on fatal errors and are not meant to be
//! used from other applications embedding the root crate. `dispatch` owns `dispatch_command()`/the `Command`
//! struct table that used to live in `src/main.rs` (issue #147) — the root's `main()` is three lines: init
//! tracing, parse `Cli`, call [`dispatch::dispatch_command`].
//!
//! ## Adding a new command
//!
//! 1. Add a variant to `Command` in `args.rs`.
//! 2. Put the command's body in `src/<command>.rs` (e.g. `serve.rs`) as a `pub fn run(...)`, and keep a struct in
//!    `dispatch.rs` that holds the pre-extracted arguments for that command.
//! 3. `impl CliCommand for YourCmd { fn execute(self) { <module>::run(...) } }` — the struct's `execute()`
//!    should stay a one-line delegating call.
//! 4. Add one arm to `dispatch_command()` in `dispatch.rs`.
//!
//! No changes to the root crate's `main()` are required.

pub mod admin_client;
pub mod args;
pub mod config_path;
pub mod dispatch;
pub mod fmt;
pub mod init;
pub mod probe;
pub mod serve;
pub mod status;
pub mod upstream_urls;
pub mod validate;

/// A CLI subcommand that can be executed.
pub trait CliCommand {
    /// Run the command.  Implementations may call `std::process::exit` on
    /// fatal errors (consistent with the binary entry-point convention).
    fn execute(self);
}

/// This crate's own view of the one feature it hosts, for the root's parity assert
/// (`src/cli/mod.rs`).
pub mod features {
    pub const KUBERNETES: bool = cfg!(feature = "kubernetes");
}
