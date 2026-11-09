use std::path::Path;
use std::process;

use super::config_path::load_config_or_exit;

// ── fmt ────────────────────────────────────────────────────────────────────

pub fn run(config_path: &str, write: bool) {
    let path = Path::new(&config_path);
    // `fmt` writes the configuration as it was loaded, that is with every `$VAR` replaced by its value: secrets
    // would land in the output, and with --write in the file, in place of the references (issue #679).
    if let Some(problem) = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| resolved_references_problem(&raw))
    {
        eprintln!("error: {problem}");
        process::exit(1);
    }
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

/// The reason `fmt` must not run on this configuration text, `None` when it may: it refers to environment variables
/// that are set (the names only are mentioned, never the values).
fn resolved_references_problem(raw: &str) -> Option<String> {
    let names = conduit_config_core::env::resolved_references(raw);
    if names.is_empty() {
        return None;
    }
    let list = names
        .iter()
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "refusing to format: the configuration refers to environment variables that are set ({list}). Formatting writes the configuration with each reference replaced by its value, which would put those values (secrets, usually) into the output, or into the file with --write. Format a copy that does not use the references, or edit the layout by hand."
    ))
}

#[cfg(test)]
mod tests {
    use super::resolved_references_problem;

    #[test]
    fn a_configuration_with_a_set_reference_is_refused_by_name_only() {
        std::env::set_var("CONDUIT_FMT_TEST_679", "super-secret-value");
        let problem = resolved_references_problem("jwtAuth:\n  secret: $CONDUIT_FMT_TEST_679\n");
        std::env::remove_var("CONDUIT_FMT_TEST_679");
        let message = problem.expect("a set reference must be refused");
        assert!(message.contains("$CONDUIT_FMT_TEST_679"), "{message}");
        assert!(!message.contains("super-secret-value"), "{message}");
    }

    #[test]
    fn an_unset_reference_and_plain_text_are_accepted() {
        // an unset variable stays `$NAME` in the output: nothing is replaced, nothing leaks
        assert!(resolved_references_problem("secret: $CONDUIT_FMT_TEST_679_UNSET\n").is_none());
        assert!(resolved_references_problem("port: 8080\nprice: $5\n").is_none());
    }
}
