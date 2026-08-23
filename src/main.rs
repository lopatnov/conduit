use std::process;

use clap::{CommandFactory, Parser};
use clap_complete::Shell as ClapShell;
use conduit::cli::args::{Cli, Command, Shell, UpstreamsCommand};
use conduit::cli::CliCommand;
use conduit::cli::{admin_client, config_path, fmt, init, probe, serve, status, validate};

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
    let config = config_path::resolve_config_path(&cli.config);

    // Kubernetes CRD mode: when --kubernetes-namespace is set and no subcommand
    // is given, start the server using the KubernetesProvider instead of a file.
    #[cfg(feature = "kubernetes")]
    if let (Some(ns), None) = (&cli.kubernetes_namespace, &cli.command) {
        serve::run_kubernetes(ns);
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
                admin_addr: admin_client::resolve_admin(args.admin.as_deref()),
            }
            .execute();
        }
        Some(Command::Status(args)) => {
            StatusCmd {
                admin_addr: admin_client::resolve_admin(args.admin.as_deref()),
                upstream: args.upstream,
            }
            .execute();
        }
        Some(Command::Shutdown(args)) => {
            ShutdownCmd {
                admin_addr: admin_client::resolve_admin(args.admin.as_deref()),
            }
            .execute();
        }
        Some(Command::Completions(args)) => CompletionsCmd { shell: args.shell }.execute(),
        Some(Command::Man) => ManCmd.execute(),
        Some(Command::Upstreams(args)) => {
            UpstreamsCmd {
                admin_addr: admin_client::resolve_admin(args.admin.as_deref()),
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
        serve::run(&self.config_path);
    }
}

struct ValidateCmd {
    config_path: String,
}
impl CliCommand for ValidateCmd {
    fn execute(self) {
        validate::run(&self.config_path);
    }
}

struct FmtCmd {
    config_path: String,
    write: bool,
}
impl CliCommand for FmtCmd {
    fn execute(self) {
        fmt::run(&self.config_path, self.write);
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
        probe::run(&self.config_path);
    }
}

struct ReloadCmd {
    admin_addr: String,
}
impl CliCommand for ReloadCmd {
    fn execute(self) {
        admin_client::admin_post("reload", &self.admin_addr);
    }
}

struct StatusCmd {
    admin_addr: String,
    upstream: bool,
}
impl CliCommand for StatusCmd {
    fn execute(self) {
        if self.upstream {
            status::print_upstream_table(&self.admin_addr);
        } else {
            admin_client::admin_get("status", &self.admin_addr);
        }
    }
}

struct ShutdownCmd {
    admin_addr: String,
}
impl CliCommand for ShutdownCmd {
    fn execute(self) {
        admin_client::admin_post("shutdown", &self.admin_addr);
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
            None => admin_client::admin_get("upstreams", &self.admin_addr),
            Some(UpstreamsCommand::Add(a)) => {
                let body = admin_client::upstream_json(
                    &a.route,
                    &a.target,
                    Some(a.weight.unwrap_or(1)),
                    a.site.as_deref(),
                );
                admin_client::admin_post_json("upstreams/add", &self.admin_addr, &body);
            }
            Some(UpstreamsCommand::Remove(r)) => {
                let body =
                    admin_client::upstream_json(&r.route, &r.target, None, r.site.as_deref());
                admin_client::admin_post_json("upstreams/remove", &self.admin_addr, &body);
            }
            Some(UpstreamsCommand::Weight(w)) => {
                let body = admin_client::upstream_json(
                    &w.route,
                    &w.target,
                    Some(w.weight),
                    w.site.as_deref(),
                );
                admin_client::admin_post_json("upstreams/weight", &self.admin_addr, &body);
            }
        }
    }
}
