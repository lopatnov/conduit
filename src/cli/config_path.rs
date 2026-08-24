use std::path::Path;
use std::process;

use crate::config::schema::AppConfig;

/// Resolve the config path, trying YAML alternatives when the default JSON
/// path does not exist.
///
/// Priority: explicit path → conduit.json → conduit.yaml → conduit.yml.
/// Only applies when the user did not pass `-c` explicitly (i.e. the value
/// is still the default "conduit.json" and that file is absent).
pub fn resolve_config_path(config_arg: &str) -> String {
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

/// Load the config at `path`, printing an error and exiting the process on
/// failure. Shared by every command whose first step is loading a config
/// file, so the missing-file hint (below) stays consistent across all of them.
pub(crate) fn load_config_or_exit(path: &Path) -> AppConfig {
    match crate::config::load_config(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error loading config: {e}");
            print_missing_config_hint(path, &e);
            process::exit(1);
        }
    }
}

/// When a config-load error's root cause is "file not found", print
/// first-run guidance (`conduit init`, `--help`). A parse/validation error
/// on an *existing* file gets no extra hint here — the underlying error
/// already names the problem.
///
/// `path` is the argument as resolved by [`resolve_config_path`]: auto-
/// discovery of the `conduit.yaml`/`conduit.yml` alternatives only ever
/// happens there when the argument is literally `"conduit.json"` (the
/// default, whether reached via `-c conduit.json` or by not passing `-c`
/// at all) — so that's the only case where it's accurate to say those
/// names were searched for. Any other path was explicitly requested and
/// wasn't found there, which is a different, more specific message.
fn print_missing_config_hint(path: &Path, e: &anyhow::Error) {
    let not_found = e
        .root_cause()
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io_e| io_e.kind() == std::io::ErrorKind::NotFound);
    if !not_found {
        return;
    }
    eprintln!();
    if path == Path::new("conduit.json") {
        eprintln!(
            "No config file found (looked for conduit.json, conduit.yaml, conduit.yml \
             in the current directory)."
        );
    } else {
        eprintln!("No config file found at {}.", path.display());
    }
    eprintln!(
        "  Run `conduit init` to create one interactively, or `conduit init -y` to accept defaults."
    );
    eprintln!("  Run `conduit --help` to see all commands and options.");
    eprintln!("  Or point to an existing config: `conduit -c /path/to/conduit.yaml`");
}
