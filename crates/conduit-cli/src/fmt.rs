use std::path::Path;
use std::process;

use super::config_path::load_config_or_exit;

// ── fmt ────────────────────────────────────────────────────────────────────

pub fn run(config_path: &str, write: bool) {
    let path = Path::new(&config_path);
    let app = load_config_or_exit(path);

    // Preserve the input format: YAML files stay YAML, JSON files stay JSON.
    let ext = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
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
