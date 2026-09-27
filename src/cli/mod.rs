//! Facade over `conduit_cli` (issue #147): `main()`'s CLI subcommand dispatch moved into `crates/conduit-cli`.
//! Every item is re-exported here at its original path, so no call site changed — `src/main.rs` and
//! `tests/cli.rs` still reach `conduit::cli::args::Cli`/`conduit::cli::dispatch::dispatch_command`/etc.

pub use conduit_cli::{
    admin_client, args, config_path, dispatch, fmt, init, probe, serve, status, upstream_urls,
    validate, CliCommand,
};

// The `Cli.kubernetes_namespace` field and `dispatch_command`'s Kubernetes-mode branch (`args.rs`/`dispatch.rs`)
// need this crate's own `kubernetes` feature; a build where it drifts from the root's would compile fine and
// either lose `--kubernetes-namespace` silently or expose a flag that panics reaching for missing code.
const _: () = assert!(
    conduit_cli::features::KUBERNETES == cfg!(feature = "kubernetes"),
    "`lopatnov-conduit-cli`'s `kubernetes` feature and the root crate's `kubernetes` feature must be enabled together"
);
