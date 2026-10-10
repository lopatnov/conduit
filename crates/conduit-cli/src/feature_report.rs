//! `conduit features`: print the Cargo features a configuration needs (issue #473).

use std::path::Path;
use std::process;

use conduit_server::config::validate::{compiled_features, required_features};

use super::config_path::load_config_or_exit;

/// The named feature bundles of the root crate, smallest first, as `(name, members)` — a copy of the
/// `[features]` table in the root `Cargo.toml` (`standard`/`gateway`/`static-server`/`full`).
const BUNDLES: &[(&str, &[&str])] = &[
    ("static-server", &["static", "compression", "hotreload"]),
    (
        "gateway",
        &[
            "proxy",
            "jwt",
            "consumers",
            "forward-auth",
            "cache",
            "acme",
            "compression",
        ],
    ),
    (
        "standard",
        &[
            "jwt",
            "consumers",
            "forward-auth",
            "cache",
            "acme",
            "proxy",
            "compression",
            "static",
            "hotreload",
        ],
    ),
    (
        "full",
        &[
            "jwt",
            "consumers",
            "forward-auth",
            "cache",
            "acme",
            "proxy",
            "compression",
            "static",
            "hotreload",
            "rhai",
            "wasm",
            "tcp",
            "upload",
            "redis",
            "fault-injection",
            "otlp",
        ],
    ),
];

/// What `conduit features` reports for one configuration.
struct Report {
    required: Vec<&'static str>,
    missing: Vec<&'static str>,
    bundle: Option<&'static str>,
}

impl Report {
    fn new(required: Vec<&'static str>, compiled: &[&str]) -> Self {
        let missing = required
            .iter()
            .copied()
            .filter(|f| !compiled.contains(f))
            .collect();
        // `cache` enables `proxy`, so a bundle must carry the proxy too.
        let covers = |members: &[&str]| {
            required.iter().all(|f| members.contains(f))
                && (!required.contains(&"cache") || members.contains(&"proxy"))
        };
        let bundle = if required.is_empty() {
            None
        } else {
            BUNDLES
                .iter()
                .find(|(_, members)| covers(members))
                .map(|(name, _)| *name)
        };
        Self {
            required,
            missing,
            bundle,
        }
    }

    fn build_line(&self) -> String {
        let mut line = String::from("cargo install lopatnov-conduit --no-default-features");
        if !self.required.is_empty() {
            line.push_str(" --features ");
            line.push_str(&self.required.join(","));
        }
        line
    }

    fn text(&self) -> String {
        let required = if self.required.is_empty() {
            "(none)".to_owned()
        } else {
            self.required.join(", ")
        };
        let mut out = format!("required: {required}\nbuild:    {}\n", self.build_line());
        if let Some(bundle) = self.bundle {
            out.push_str(&format!(
                "bundle:   `--features {bundle}` also covers this configuration\n"
            ));
        }
        out
    }

    fn json(&self) -> String {
        serde_json::json!({
            "required": self.required,
            "build": self.build_line(),
            "bundle": self.bundle,
            "missing": self.missing,
        })
        .to_string()
    }
}

/// Print the features `config_path` needs; exit non-zero when this binary lacks one of them.
pub fn run(config_path: &str, json: bool) {
    let app = load_config_or_exit(Path::new(config_path));
    let report = Report::new(required_features(&app), &compiled_features());
    if json {
        println!("{}", report.json());
    } else {
        print!("{}", report.text());
    }
    if !report.missing.is_empty() {
        eprintln!(
            "error: this binary was built without: {}",
            report.missing.join(", ")
        );
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_output_is_pinned() {
        let report = Report::new(vec!["forward-auth", "jwt"], &["jwt"]);
        assert_eq!(
            report.text(),
            "required: forward-auth, jwt\n\
             build:    cargo install lopatnov-conduit --no-default-features --features forward-auth,jwt\n\
             bundle:   `--features gateway` also covers this configuration\n"
        );
        assert_eq!(report.missing, vec!["forward-auth"]);
    }

    #[test]
    fn json_output_is_pinned() {
        let report = Report::new(vec!["otlp"], &["otlp", "proxy"]);
        assert_eq!(
            report.json(),
            r#"{"build":"cargo install lopatnov-conduit --no-default-features --features otlp","bundle":"full","missing":[],"required":["otlp"]}"#
        );
    }

    #[test]
    fn a_config_that_needs_nothing_prints_an_empty_set() {
        let report = Report::new(Vec::new(), &[]);
        assert_eq!(
            report.text(),
            "required: (none)\nbuild:    cargo install lopatnov-conduit --no-default-features\n"
        );
        assert!(report.missing.is_empty());
        assert_eq!(report.bundle, None);
    }

    #[test]
    fn a_bundle_for_cache_must_carry_proxy() {
        // `static-server` has no proxy, so it can never be offered for a cache config.
        let report = Report::new(vec!["cache"], &[]);
        assert_ne!(report.bundle, Some("static-server"));
        assert_eq!(report.bundle, Some("gateway"));
    }
}
