//! `conduit features`: print the Cargo features a configuration needs (issue #473).

use std::path::Path;
use std::process;

use conduit_server::config::validate::{compiled_features, required_features};

use super::config_path::load_config_or_exit_with;

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

/// Exit status when the binary lacks a feature the configuration needs.
const EXIT_MISSING_FEATURE: i32 = 1;
/// Exit status when the configuration could not be loaded: a script can tell it from a missing feature.
const EXIT_CONFIG_NOT_LOADED: i32 = 2;

/// Print the features `config_path` needs; exit 1 when this binary lacks one of them, 2 when the
/// configuration cannot be loaded.
pub fn run(config_path: &str, json: bool) {
    let app = load_config_or_exit_with(Path::new(config_path), EXIT_CONFIG_NOT_LOADED);
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
        process::exit(EXIT_MISSING_FEATURE);
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

    /// The `[features]` table of the root `Cargo.toml` as `name -> members` (a line parser: the table uses only
    /// one-name-per-entry arrays of quoted strings, with `#` comments).
    fn root_features() -> std::collections::HashMap<String, Vec<String>> {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let text = std::fs::read_to_string(format!("{manifest}/../../Cargo.toml"))
            .expect("root Cargo.toml");
        let mut table = std::collections::HashMap::new();
        let (mut in_features, mut current): (bool, Option<(String, Vec<String>)>) = (false, None);
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') {
                in_features = line == "[features]";
                continue;
            }
            if !in_features || line.is_empty() {
                continue;
            }
            if current.is_none() {
                if let Some((name, rest)) = line.split_once('=') {
                    current = Some((name.trim().to_owned(), Vec::new()));
                    let members = rest.trim_start_matches([' ', '[']);
                    push_members(&mut current, members);
                }
            } else {
                push_members(&mut current, line);
            }
            if line.ends_with(']') {
                let (name, members) = current.take().expect("an open entry");
                table.insert(name, members);
            }
        }
        table
    }

    fn push_members(current: &mut Option<(String, Vec<String>)>, text: &str) {
        let Some((_, members)) = current else { return };
        members.extend(text.split('"').skip(1).step_by(2).map(str::to_owned));
    }

    /// The members of `name`, with the bundles inside it expanded.
    fn expanded(table: &std::collections::HashMap<String, Vec<String>>, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        for member in &table[name] {
            if table.contains_key(member)
                && ["standard", "gateway", "static-server", "full"].contains(&member.as_str())
            {
                out.extend(expanded(table, member));
            } else {
                out.push(member.clone());
            }
        }
        out
    }

    /// `BUNDLES` is a copy of the root `[features]` table (issue #562): when a bundle gains or loses a member
    /// there, `conduit features` would name the wrong bundle. The features of `full` that no configuration
    /// needs (a marker, a metrics gauge, a start-up flag) are the only ones `BUNDLES` leaves out.
    #[test]
    fn bundles_agree_with_the_root_cargo_toml() {
        const NOT_REPORTED: &[&str] = &["disk-cache", "tokio-metrics", "kubernetes"];
        let table = root_features();
        for (name, members) in BUNDLES {
            let mut cargo: Vec<String> = expanded(&table, name)
                .into_iter()
                .filter(|f| !NOT_REPORTED.contains(&f.as_str()))
                .collect();
            let mut ours: Vec<String> = members.iter().map(|m| (*m).to_owned()).collect();
            cargo.sort();
            cargo.dedup();
            ours.sort();
            assert_eq!(
                ours, cargo,
                "BUNDLES[{name}] differs from Cargo.toml [features]"
            );
        }
    }

    #[test]
    fn a_bundle_for_cache_must_carry_proxy() {
        // `static-server` has no proxy, so it can never be offered for a cache config.
        let report = Report::new(vec!["cache"], &[]);
        assert_ne!(report.bundle, Some("static-server"));
        assert_eq!(report.bundle, Some("gateway"));
    }
}
