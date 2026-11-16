//! Kubernetes provider — watch `ConduitSite` CRDs and stream config updates.
//!
//! Enabled with the `kubernetes` feature flag:
//! ```bash
//! cargo build --features kubernetes
//! ```
//!
//! ## Custom Resource Definition
//!
//! Each `ConduitSite` resource in Kubernetes maps to one [`SiteConfig`].
//! The provider lists all `ConduitSite` resources in the configured namespace,
//! builds an [`AppConfig`], and sends it. It then watches for `Added`,
//! `Modified`, and `Deleted` events and re-sends an updated config on each.
//!
//! Example resource:
//! ```yaml
//! apiVersion: conduit.lopatnov.github.io/v1
//! kind: ConduitSite
//! metadata:
//!   name: my-app
//!   namespace: default
//! spec:
//!   port: 8080
//!   proxy: "http://my-svc:4000"
//!   healthCheck: true
//! ```
//!
//! Install the CRD with:
//! ```bash
//! kubectl apply -f contrib/k8s/conduitsite-crd.yaml
//! ```
//!
//! The generic list+watch mechanism (`KubernetesProvider<B>`, the CRD types,
//! and the watch machinery) lives in the Layer-0 [`conduit_k8s`] crate
//! (issue #114/#249) — it cannot know about [`AppConfig`]/[`SiteConfig`]
//! without creating a dependency cycle (see `CONTRIBUTING.md`'s crate
//! extraction recipe, rule 2: "generic-in-crate, bound-by-type-alias-in-
//! root"). This module binds the generic mechanism to conduit's real schema
//! via [`ConduitSchema`] and re-exports everything at its original path so
//! `crate::config::kubernetes::*` call sites (`src/cli/serve.rs`) don't change.

use anyhow::Result;

use conduit_k8s::CrdConfigBuilder;

use crate::config::schema::{AdminConfig, AppConfig, GlobalConfig, SiteConfig};

pub use conduit_k8s::{ConduitSite, ConduitSiteSpec, ConduitSiteStatus};

/// Binds [`conduit_k8s::CrdConfigBuilder`] to conduit's real config schema.
///
/// `site_from_spec` relies on [`ConduitSiteSpec`] using the same field names
/// (and serde renames) as [`SiteConfig`], so serializing to JSON and
/// deserializing as `SiteConfig` produces the correct result.
pub struct ConduitSchema;

impl CrdConfigBuilder for ConduitSchema {
    type Site = SiteConfig;
    type Config = AppConfig;

    fn site_from_spec(spec: &ConduitSiteSpec) -> Result<SiteConfig> {
        let json = serde_json::to_value(spec)?;
        reject_host_paths(&json)?;
        let site: SiteConfig = serde_json::from_value(json)?;
        // A `disk:<path>` cache store writes upstream responses under a directory of the host: a host
        // path like the ones above, in a field the key-by-key check cannot see (the route shapes differ).
        if crate::config::validate::site_uses_disk_store(&site) {
            anyhow::bail!(
                "host file paths cannot be set from a ConduitSite (cache.store disk:<path>); \
                 they are only allowed in a file config"
            );
        }
        // The Redis address belongs to the operator of the process (`CONDUIT_REDIS_URL`): a site only says
        // `store: redis`. A site that names an address would put its counters, or the cached responses of
        // another site, in a Redis of its writer's choice.
        if crate::config::validate::site_names_redis_address(&site) {
            anyhow::bail!(
                "a ConduitSite cannot name a Redis address (store: redis://...); write `store: redis` and let \
                 the operator set the address once (CONDUIT_REDIS_URL in the pod)"
            );
        }
        // What is wrong with this site alone (a hard validation error) is refused here, so that the
        // provider can leave this one object out instead of the whole config being refused.
        let alone = AppConfig {
            sites: vec![site.clone()],
            ..Default::default()
        };
        let (_, hard) = crate::config::validate::partition_by_severity(
            crate::config::validate::validate(&alone),
        );
        if !hard.is_empty() {
            let errors: Vec<String> = hard
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect();
            anyhow::bail!("{}", errors.join("; "));
        }
        Ok(site)
    }

    fn placeholder(spec: &ConduitSiteSpec, others: &[SiteConfig]) -> Option<SiteConfig> {
        // No host (or `*`): the site was a catch-all, and without an answer of its own its requests would go to
        // the first site of the port.
        let host = spec.host.as_deref().filter(|h| !h.is_empty() && *h != "*");
        let port = spec
            .port
            .unwrap_or(if spec.tls.is_some() { 443 } else { 80 });
        let same_port: Vec<&SiteConfig> = others
            .iter()
            .filter(|o| o.port.unwrap_or(if o.tls.is_some() { 443 } else { 80 }) == port)
            .collect();
        // On a port of its own nothing falls through to another site: the listener is not bound and the
        // connection is refused. (A new port would also be a restart-only change that refuses the whole update.)
        if same_port.is_empty() {
            return None;
        }
        // A port of a `tcp` site, or a host and port that another site already has, would make the whole config
        // invalid (`validate_no_duplicate_host_port`): that site is left out as before.
        let taken = same_port.iter().any(|o| {
            o.tcp.is_some()
                || match (host, o.host.as_deref()) {
                    (Some(h), Some(oh)) => h.eq_ignore_ascii_case(oh),
                    (None, None | Some("*")) => true,
                    _ => false,
                }
        });
        if taken {
            return None;
        }
        let site: SiteConfig = serde_json::from_value(serde_json::json!({
            "host": host,
            "port": port,
            "fallback": { "status": 503, "body": "ConduitSite is not valid" },
        }))
        .ok()?;
        // Whatever stops it standing alone leaves the object out as before.
        let alone = AppConfig {
            sites: vec![site.clone()],
            ..Default::default()
        };
        let (_, hard) = crate::config::validate::partition_by_severity(
            crate::config::validate::validate(&alone),
        );
        hard.is_empty().then_some(site)
    }

    fn build_config(sites: Vec<SiteConfig>, admin_bind: &str) -> AppConfig {
        AppConfig {
            global: Some(GlobalConfig {
                admin: Some(AdminConfig {
                    bind: Some(admin_bind.to_owned()),
                    token: Some(admin_token()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            sites,
            ..Default::default()
        }
    }
}

/// The Admin API token of a pod that takes its config from `ConduitSite` objects.
///
/// The Admin API is on loopback, but a `ConduitSite` may route to any address the pod can reach, loopback included
/// (`proxy: http://127.0.0.1:2019`, or a `tcp` target): without a token, whoever writes a `ConduitSite` could call
/// `POST /shutdown` or `POST /upstreams/add` through their own site. So the API always has one: `CONDUIT_ADMIN_TOKEN`
/// from the pod's environment when set (the variable `conduit status` and the other admin commands read, so they work
/// in the pod), otherwise a random one that nobody knows, which closes the API. It is made once per process: a config
/// rebuilt on every `ConduitSite` change must not rotate it.
fn admin_token() -> String {
    static TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOKEN
        .get_or_init(|| {
            match token_from_env_value(std::env::var("CONDUIT_ADMIN_TOKEN").ok().as_deref()) {
                Some(token) => {
                    if token.len() < MIN_ADMIN_TOKEN_LEN {
                        tracing::warn!(
                            "CONDUIT_ADMIN_TOKEN is shorter than {MIN_ADMIN_TOKEN_LEN} characters: use a long random value, \
                             the Admin API has no limit on wrong guesses"
                        );
                    }
                    token
                }
                None => random_token(),
            }
        })
        .clone()
}

/// A shorter `CONDUIT_ADMIN_TOKEN` is warned about at start.
const MIN_ADMIN_TOKEN_LEN: usize = 32;

/// The token in the value of `CONDUIT_ADMIN_TOKEN`, if there is one. Surrounding whitespace is dropped: a `Secret`
/// mounted from a file ends in a newline, which the server and the admin commands would not accept as part of a token.
fn token_from_env_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// 32 random bytes from the system's secure random source, as 64 hex characters.
fn random_token() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut bytes)
        .expect("the system random source is available");
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// CRD-mode trust boundary (#580): whoever can write a `ConduitSite` must not be
/// able to name files or directories on the Conduit host. A spec that sets any
/// path-valued field is refused, so a tenant cannot read, write or serve
/// arbitrary host paths. Certificates come from `tls.acme` (storage stays at the
/// operator's default) and static content from upstreams, not host directories.
fn reject_host_paths(spec: &serde_json::Value) -> Result<()> {
    fn check(scope: &str, v: &serde_json::Value, out: &mut Vec<String>) {
        let has = |p: &[&str]| {
            let mut cur = v;
            for k in p {
                match cur.get(*k) {
                    Some(n) if !n.is_null() => cur = n,
                    _ => return false,
                }
            }
            true
        };
        for p in [
            &["static"][..],
            &["upload"],
            &["middleware"],
            &["tls", "cert"],
            &["tls", "key"],
            &["tls", "ca"],
            &["tls", "clientAuth"],
            &["tls", "acme", "storage"],
            &["logging", "file"],
            &["fallback", "file"],
        ] {
            if has(p) {
                out.push(format!("{scope}{}", p.join(".")));
            }
        }
        if let Some(by) = v.pointer("/fallback/byAccept").and_then(|b| b.as_object()) {
            if by
                .values()
                .any(|e| e.get("file").is_some_and(|f| !f.is_null()))
            {
                out.push(format!("{scope}fallback.byAccept.*.file"));
            }
        }
    }
    let mut bad = Vec::new();
    check("", spec, &mut bad);
    if let Some(routes) = spec.get("routes").and_then(|r| r.as_array()) {
        for (i, r) in routes.iter().enumerate() {
            check(&format!("routes[{i}]."), r, &mut bad);
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "host file paths cannot be set from a ConduitSite ({}); they are only allowed in a file config",
            bad.join(", ")
        )
    }
}

/// Stream [`AppConfig`] updates from `ConduitSite` Kubernetes CRDs.
///
/// See [`conduit_k8s::KubernetesProvider`] for the generic mechanism
/// (list+watch, namespace handling, error recovery).
pub type KubernetesProvider = conduit_k8s::KubernetesProvider<ConduitSchema>;

/// Convert a [`ConduitSiteSpec`] to a [`SiteConfig`] via JSON round-trip.
///
/// Thin wrapper over [`ConduitSchema::site_from_spec`] kept at its original
/// name and signature (recipe rule 3) — `conduit_k8s::build_app_config` is
/// generic over the schema binding, so a direct re-export can't preserve the
/// original turbofish-free call signature the way this wrapper does.
pub fn spec_to_site_config(spec: &ConduitSiteSpec) -> Result<SiteConfig> {
    ConduitSchema::site_from_spec(spec)
}

/// Build an [`AppConfig`] from a list of `ConduitSite` CRDs.
///
/// Thin wrapper over [`conduit_k8s::build_app_config`] bound to
/// [`ConduitSchema`] — same reasoning as [`spec_to_site_config`] above.
pub fn build_app_config<'a>(
    sites: impl Iterator<Item = &'a ConduitSite>,
    admin_bind: &str,
) -> Result<AppConfig> {
    conduit_k8s::build_app_config::<ConduitSchema>(sites, admin_bind)
}

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// These cover the *schema-specific* behavior of `ConduitSchema`'s JSON
// round-trip against the real `SiteConfig`/`AppConfig` types — the generic
// list+watch mechanism's own tests (constructor/builder fields, generic
// error-attribution) moved to `crates/conduit-k8s/src/provider.rs`, which
// can't test against the real schema without creating a dependency cycle.

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spec(port: u16, host: Option<&str>) -> ConduitSiteSpec {
        ConduitSiteSpec {
            port: Some(port),
            host: host.map(str::to_owned),
            proxy: None,
            static_files: None,
            health_check: None,
            tls: None,
            headers: None,
            fallback: None,
            logging: None,
            compression: None,
            rate_limit: None,
            basic_auth: None,
            api_key: None,
            ip_filter: None,
            cors: None,
            metrics: None,
            upload: None,
            redirects: None,
            middleware: None,
            routes: None,
            extra: Default::default(),
        }
    }

    fn make_crd(name: &str, spec: ConduitSiteSpec) -> ConduitSite {
        ConduitSite::new(name, spec)
    }

    // ── spec_to_site_config ───────────────────────────────────────────────────

    #[test]
    fn crd_specs_cannot_name_host_paths() {
        for (field, value) in [
            ("static", serde_json::json!({"root": "/etc"})),
            ("upload", serde_json::json!({"path": "/u", "dir": "/etc"})),
            (
                "tls",
                serde_json::json!({"cert": "/etc/ssl/c.pem", "key": "/k"}),
            ),
            (
                "tls",
                serde_json::json!({"acme": {"email": "a@b.c", "storage": "../../x"}}),
            ),
            ("logging", serde_json::json!({"file": "/etc/cron.d/x"})),
            ("fallback", serde_json::json!({"file": "/etc/passwd"})),
            (
                "middleware",
                serde_json::json!([{"type": "wasm", "path": "/x.wasm"}]),
            ),
        ] {
            let mut spec = make_spec(8080, Some("a.example.com"));
            match field {
                "static" => spec.static_files = Some(value),
                "upload" => spec.upload = Some(value),
                "tls" => spec.tls = Some(value),
                "logging" => spec.logging = Some(value),
                "fallback" => spec.fallback = Some(value),
                _ => spec.middleware = Some(value),
            }
            let err = spec_to_site_config(&spec).expect_err(field).to_string();
            assert!(err.contains("host file paths"), "{field}: {err}");
        }
    }

    #[test]
    fn crd_spec_in_a_route_cannot_name_host_paths() {
        let mut spec = make_spec(8080, None);
        spec.routes = Some(serde_json::json!([{"path": "/", "static": {"root": "/etc"}}]));
        assert!(spec_to_site_config(&spec).is_err());
    }

    // ── a guard for the deny-list (issue #587) ──────────────────────────────────

    /// The paths of a site that `reject_host_paths` refuses (`*` is any key of a map).
    const DENIED: &[&str] = &[
        "static",
        "upload",
        "middleware",
        "tls.cert",
        "tls.key",
        "tls.ca",
        "tls.clientAuth",
        "tls.acme.storage",
        "logging.file",
        "fallback.file",
        "fallback.byAccept.*.file",
    ];

    /// Fields named like a path that are URL paths, not host paths: they may be set from a `ConduitSite`.
    const URL_PATHS: &[&str] = &[
        "healthCheck.path",
        "proxy.*.healthCheck.path",
        "proxy.healthCheck.path",
        "match.path",
        "metrics.path",
    ];

    /// Every property name of the JSON schema, as a dotted path (`sites.*.tls.cert`), following `$ref`.
    fn schema_paths(
        node: &serde_json::Value,
        defs: &serde_json::Value,
        path: &[String],
        seen: &[String],
        out: &mut Vec<String>,
    ) {
        if let Some(props) = node.get("properties").and_then(|p| p.as_object()) {
            for (name, child) in props {
                let mut here = path.to_vec();
                here.push(name.clone());
                out.push(here.join("."));
                schema_paths(child, defs, &here, seen, out);
            }
        }
        for key in ["items", "additionalProperties"] {
            if node.get(key).is_some_and(|c| c.is_object()) {
                let mut here = path.to_vec();
                here.push("*".to_owned());
                schema_paths(&node[key], defs, &here, seen, out);
            }
        }
        for key in ["oneOf", "anyOf", "allOf"] {
            for child in node
                .get(key)
                .and_then(|c| c.as_array())
                .into_iter()
                .flatten()
            {
                schema_paths(child, defs, path, seen, out);
            }
        }
        if let Some(reference) = node.get("$ref").and_then(|r| r.as_str()) {
            let name = reference.rsplit('/').next().unwrap_or(reference).to_owned();
            if !seen.contains(&name) {
                let mut seen = seen.to_vec();
                seen.push(name.clone());
                schema_paths(&defs[&name], defs, path, &seen, out);
            }
        }
    }

    fn is_path_like(name: &str) -> bool {
        matches!(
            name,
            "file"
                | "dir"
                | "path"
                | "cert"
                | "key"
                | "ca"
                | "root"
                | "storage"
                | "static"
                | "upload"
                | "middleware"
                | "clientAuth"
        ) || name.ends_with("File")
            || name.ends_with("Dir")
            || name.ends_with("Path")
    }

    fn covered(path: &str, list: &[&str]) -> bool {
        list.iter()
            .any(|d| path == *d || path.starts_with(&format!("{d}.")))
    }

    /// `reject_host_paths` is a deny-list: a new path-valued field of a site must be added to it by hand. This test
    /// walks the JSON schema and fails for a field that is named like a path (file, dir, cert, key, ca, storage, root,
    /// `...File`, `...Dir`, `...Path`) and is neither refused nor listed as a URL path, so that a field added to
    /// the schema cannot slip through.
    #[test]
    fn every_path_like_field_of_the_schema_is_refused_or_known_to_be_a_url_path() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../../schema/conduit.schema.json")).unwrap();
        let mut all = Vec::new();
        schema_paths(&schema, &schema["$defs"], &[], &[], &mut all);

        let mut uncovered = Vec::new();
        let mut checked = 0;
        for full in &all {
            // relative to a site, and a route is a site for this purpose
            let rel = full
                .strip_prefix("sites.*.")
                .or_else(|| full.strip_prefix("*."))
                .unwrap_or(full);
            let rel = rel.strip_prefix("routes.*.").unwrap_or(rel);
            let name = rel.rsplit('.').next().unwrap_or(rel);
            if !is_path_like(name) {
                continue;
            }
            checked += 1;
            if !covered(rel, DENIED) && !URL_PATHS.contains(&rel) {
                uncovered.push(full.clone());
            }
        }
        assert!(
            checked >= 15,
            "the walk found only {checked} path-like fields: is the schema read right?"
        );
        uncovered.sort();
        uncovered.dedup();
        assert!(
            uncovered.is_empty(),
            "path-like fields neither refused by reject_host_paths nor listed as URL paths: {uncovered:?}. \
             If a ConduitSite must not set the field, add it to reject_host_paths and to DENIED; if it is a URL path, \
             add it to URL_PATHS."
        );
    }

    /// The `DENIED` list of the guard is what `reject_host_paths` really refuses.
    #[test]
    fn the_guard_list_matches_reject_host_paths() {
        for path in DENIED {
            let mut spec = serde_json::Value::String("/etc/x".to_owned());
            for key in path.rsplit('.') {
                let key = if key == "*" { "any" } else { key };
                spec = serde_json::json!({ key: spec });
            }
            assert!(
                reject_host_paths(&spec).is_err(),
                "{path} must be refused: {spec}"
            );
        }
        // and a URL path is not
        let spec =
            serde_json::json!({ "healthCheck": { "path": "/health" }, "match": { "path": "/x" } });
        assert!(reject_host_paths(&spec).is_ok());
    }

    #[test]
    fn crd_acme_without_storage_is_allowed() {
        let mut spec = make_spec(8080, Some("a.example.com"));
        spec.tls = Some(serde_json::json!({"acme": {"email": "a@b.c"}}));
        assert!(spec_to_site_config(&spec).is_ok());
    }

    #[test]
    fn spec_to_site_config_port_and_host() {
        let spec = make_spec(8080, Some("app.example.com"));
        let site = spec_to_site_config(&spec).expect("conversion must succeed");
        assert_eq!(site.port, Some(8080));
        assert_eq!(site.host.as_deref(), Some("app.example.com"));
    }

    #[test]
    fn spec_to_site_config_minimal_defaults() {
        let spec = make_spec(3000, None);
        let site = spec_to_site_config(&spec).expect("conversion must succeed");
        assert_eq!(site.port, Some(3000));
        assert!(site.host.is_none());
        assert!(site.proxy.is_none());
        assert!(site.health_check.is_none());
    }

    #[test]
    fn spec_to_site_config_with_health_check() {
        let mut spec = make_spec(8080, None);
        spec.health_check = Some(serde_json::json!(true));
        let site = spec_to_site_config(&spec).expect("conversion must succeed");
        assert!(site.health_check.is_some());
    }

    #[test]
    fn spec_to_site_config_with_proxy_string() {
        let mut spec = make_spec(8080, None);
        spec.proxy = Some(serde_json::json!("http://backend:4000"));
        let site = spec_to_site_config(&spec).expect("conversion must succeed");
        assert!(site.proxy.is_some());
    }

    // ── what a ConduitSite accepts, checked against the manifests we ship and document ──

    /// Every `kind: ConduitSite` document of a YAML text.
    fn crds_in(yaml: &str) -> Vec<ConduitSite> {
        use serde::Deserialize as _;
        serde_yaml::Deserializer::from_str(yaml)
            .map(|doc| serde_yaml::Value::deserialize(doc).expect("valid YAML"))
            .filter(|v| v["kind"] == "ConduitSite")
            .map(|v| serde_yaml::from_value(v).expect("a valid ConduitSite"))
            .collect()
    }

    /// The text of every fenced ```yaml block of a Markdown file.
    fn yaml_blocks(markdown: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut current: Option<String> = None;
        for line in markdown.lines() {
            match (&mut current, line.trim_end()) {
                (None, "```yaml") => current = Some(String::new()),
                (Some(_), "```") => blocks.extend(current.take()),
                (Some(text), l) => {
                    text.push_str(l);
                    text.push('\n');
                }
                _ => {}
            }
        }
        blocks
    }

    /// The config a set of manifests builds, with its hard validation errors as a message.
    fn check_manifests(what: &str, crds: &[ConduitSite]) {
        for crd in crds {
            let name = crd.metadata.name.as_deref().unwrap_or("?");
            spec_to_site_config(&crd.spec)
                .unwrap_or_else(|e| panic!("{what}: ConduitSite {name} is refused: {e}"));
        }
        let cfg = build_app_config(crds.iter(), "127.0.0.1:2019")
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        let (_, hard) =
            crate::config::validate::partition_by_severity(crate::config::validate::validate(&cfg));
        assert!(
            hard.is_empty(),
            "{what}: the config built from the manifests has errors: {:?}",
            hard.iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
        );
        for site in &cfg.sites {
            assert!(
                site.extra.is_empty(),
                "{what}: unknown site keys {:?}",
                site.extra.keys().collect::<Vec<_>>()
            );
        }
    }

    /// The example manifests in `contrib/k8s/` are what a user applies first: they must be accepted.
    #[test]
    fn the_shipped_example_manifests_are_accepted() {
        let crds = crds_in(include_str!("../../../../contrib/k8s/example-site.yaml"));
        assert!(!crds.is_empty());
        check_manifests("contrib/k8s/example-site.yaml", &crds);
    }

    /// Every `ConduitSite` in the docs builds a valid config (an excerpt that is not a whole
    /// manifest is not a `ConduitSite` document and is left out).
    #[test]
    fn the_documented_conduitsite_manifests_are_accepted() {
        let mut found = 0;
        for (file, text) in [
            (
                "docs/deployment.md",
                include_str!("../../../../docs/deployment.md"),
            ),
            (
                "docs/kubernetes.md",
                include_str!("../../../../docs/kubernetes.md"),
            ),
        ] {
            // one config per file: two examples may share a host and port
            for (i, block) in yaml_blocks(text).iter().enumerate() {
                let crds = crds_in(block);
                found += crds.len();
                if !crds.is_empty() {
                    check_manifests(&format!("{file}, YAML block {i}"), &crds);
                }
            }
        }
        assert!(
            found >= 4,
            "found only {found} ConduitSite documents in the docs"
        );
    }

    /// A `ConduitSite` accepts every field of a site, not only the ones its Rust type names
    /// (before, `jwtAuth` was dropped without a word and the site was served open).
    #[test]
    fn crd_spec_keeps_the_fields_it_does_not_name() {
        let crds = crds_in(
            "apiVersion: conduit.lopatnov.github.io/v1\nkind: ConduitSite\nmetadata: {name: a}\nspec:\n  port: 8080\n  \
             proxy: http://b:1\n  jwtAuth: {jwksUrl: 'https://auth.example.com/jwks.json'}\n  \
             securityHeaders: true\n  limits: {maxBodyBytes: 1000}\n  maskErrors: true\n",
        );
        let site = spec_to_site_config(&crds[0].spec).unwrap();
        assert!(site.jwt_auth.is_some(), "jwtAuth was dropped");
        assert!(
            site.security_headers.is_some(),
            "securityHeaders was dropped"
        );
        assert!(site.limits.is_some(), "limits was dropped");
        assert_eq!(site.mask_errors, Some(true));
        assert!(site.extra.is_empty());
    }

    /// A key no site field matches ends in `SiteConfig.extra`, where validation warns about it as it
    /// does for a config file.
    #[test]
    fn crd_spec_reports_a_key_no_site_field_matches() {
        let crds = crds_in(
            "apiVersion: conduit.lopatnov.github.io/v1\nkind: ConduitSite\nmetadata: {name: a}\nspec:\n  port: 8080\n  jwtAuht: {}\n",
        );
        let site = spec_to_site_config(&crds[0].spec).unwrap();
        assert!(site.extra.contains_key("jwtAuht"));
    }

    /// A `disk:<path>` cache store is a host path, in any route shape; `memory` is not (a Redis address is refused too, see `crd_spec_cannot_name_a_redis_address`).
    #[test]
    fn crd_spec_cannot_cache_to_a_host_directory() {
        let site_with = |proxy: &str| {
            let yaml = format!(
                "apiVersion: conduit.lopatnov.github.io/v1\nkind: ConduitSite\nmetadata: {{name: a}}\nspec:\n  port: 8080\n{proxy}"
            );
            spec_to_site_config(&crds_in(&yaml)[0].spec)
        };
        let in_map = |store: &str| {
            format!("  proxy:\n    /:\n      targets: [http://b:1]\n      cache: {{store: '{store}'}}\n")
        };
        let in_routes = |store: &str| {
            format!(
                "  routes:\n    - match: {{path: /}}\n      proxy:\n        targets: [http://b:1]\n        cache: {{store: '{store}'}}\n"
            )
        };
        for store in ["disk:/etc", "disk:./cache"] {
            for (shape, proxy) in [("proxy map", in_map(store)), ("routes[]", in_routes(store))] {
                let err = site_with(&proxy)
                    .expect_err(&format!("{shape} {store}"))
                    .to_string();
                assert!(err.contains("host file paths"), "{shape} {store}: {err}");
            }
        }
        assert!(site_with(&in_map("memory")).is_ok(), "proxy map memory");
        assert!(site_with(&in_routes("memory")).is_ok(), "routes[] memory");
    }

    /// The Redis address is the operator's (issue #760): a `ConduitSite` writes `store: redis`, never an address, in
    /// a limit or a cache, in any route shape. (`store: redis` itself needs `CONDUIT_REDIS_URL` in the pod, so it
    /// is refused here, where the test environment has none; the message says what to set.)
    #[test]
    fn crd_spec_cannot_name_a_redis_address() {
        // hermetic: this process must have no operator address, whatever the developer's shell exports
        std::env::remove_var(conduit_config_core::scheme::REDIS_URL_ENV);
        let site_with = |body: &str| {
            let yaml = format!(
                "apiVersion: conduit.io/v1\nkind: ConduitSite\nmetadata: {{name: a}}\nspec:\n  port: 8080\n{body}"
            );
            spec_to_site_config(&crds_in(&yaml)[0].spec)
        };
        let bodies = [
            "  proxy: http://b:1\n  rateLimit: {{windowSecs: 60, limit: 5, store: '@STORE@'}}\n",
            "  proxy:\n    /:\n      targets: [http://b:1]\n      rateLimit: {{windowSecs: 60, limit: 5, store: '@STORE@'}}\n",
            "  proxy:\n    /:\n      targets: [http://b:1]\n      cache: {{store: '@STORE@'}}\n",
            "  routes:\n    - match: {{path: /}}\n      proxy:\n        targets: [http://b:1]\n        cache: {{store: '@STORE@'}}\n",
            "  routes:\n    - match: {{path: /}}\n      proxy:\n        targets: [http://b:1]\n        rateLimit: {{windowSecs: 60, limit: 5, store: '@STORE@'}}\n",
        ];
        for body in bodies {
            let body = body.replace("{{", "{").replace("}}", "}");
            for address in ["redis://evil.example:6379", "rediss://evil.example:6380"] {
                let err = site_with(&body.replace("@STORE@", address))
                    .expect_err(&format!("{address} in {body}"))
                    .to_string();
                assert!(err.contains("cannot name a Redis address"), "{err}");
            }
            // the bare `redis` is the allowed form; this process has no operator address, so it says what to set
            let err = site_with(&body.replace("@STORE@", "redis"))
                .expect_err("no operator address in the test environment")
                .to_string();
            assert!(err.contains("CONDUIT_REDIS_URL"), "{err}");
        }
    }

    /// The Admin API of a CRD-mode pod always has a token: a `ConduitSite` can route to loopback.
    #[test]
    fn the_admin_api_of_a_crd_pod_has_a_token_that_does_not_change() {
        let token_of = |cfg: &AppConfig| {
            cfg.global
                .as_ref()
                .and_then(|g| g.admin.as_ref())
                .and_then(|a| a.token.clone())
        };
        let a = build_app_config(std::iter::empty(), "127.0.0.1:2019").unwrap();
        let b = build_app_config(std::iter::empty(), "127.0.0.1:2019").unwrap();
        let token = token_of(&a).expect("a token is set");
        assert!(
            token.len() >= 32,
            "token too short: {} characters",
            token.len()
        );
        assert_eq!(
            Some(token),
            token_of(&b),
            "the token must not rotate with the config"
        );
        let (_, hard) =
            crate::config::validate::partition_by_severity(crate::config::validate::validate(&a));
        assert!(hard.is_empty(), "{hard:?}");
    }

    #[test]
    fn an_env_admin_token_loses_its_whitespace_and_empty_means_random() {
        assert_eq!(token_from_env_value(Some("abc\n")).as_deref(), Some("abc"));
        assert_eq!(
            token_from_env_value(Some("  a b \r\n")).as_deref(),
            Some("a b")
        );
        assert_eq!(token_from_env_value(Some(" \n")), None);
        assert_eq!(token_from_env_value(Some("")), None);
        assert_eq!(token_from_env_value(None), None);
    }

    /// One `ConduitSite` that does not convert, or is invalid by itself, is left out; the others are built.
    #[test]
    fn a_site_with_a_hard_error_of_its_own_is_refused_alone() {
        let ok = crds_in(
            "apiVersion: conduit.lopatnov.github.io/v1\nkind: ConduitSite\nmetadata: {name: ok}\nspec: {port: 8080, proxy: 'http://a:1'}\n",
        );
        // a proxy target that is not a URL is a hard validation error of the site itself
        let bad = crds_in(
            "apiVersion: conduit.lopatnov.github.io/v1\nkind: ConduitSite\nmetadata: {name: bad}\nspec: {port: 8081, proxy: 'not-a-url'}\n",
        );
        assert!(spec_to_site_config(&ok[0].spec).is_ok());
        let err = spec_to_site_config(&bad[0].spec)
            .expect_err("must be refused")
            .to_string();
        assert!(err.contains("proxy"), "{err}");
        let all = [ok[0].clone(), bad[0].clone()];
        let (cfg, _) =
            conduit_k8s::SiteSet::<ConduitSchema>::new().build(all.iter(), "127.0.0.1:2019");
        assert_eq!(cfg.sites.len(), 1);
        assert_eq!(cfg.sites[0].port, Some(8080));
    }

    /// A site that is left out must not hand its host to another site of the same port: `find_site_idx` falls back
    /// to the first site for an unknown host, and that is somebody else's upstream.
    #[test]
    fn a_dropped_site_does_not_hand_its_host_to_another_site() {
        let all = crds_in(
            "apiVersion: conduit.lopatnov.github.io/v1\nkind: ConduitSite\nmetadata: {name: other}\nspec: {host: other.example.com, port: 8080, proxy: 'http://secret-upstream:1'}\n---\n\
             apiVersion: conduit.lopatnov.github.io/v1\nkind: ConduitSite\nmetadata: {name: bad}\nspec: {host: bad.example.com, port: 8080, proxy: 'not-a-url'}\n---\n\
             apiVersion: conduit.lopatnov.github.io/v1\nkind: ConduitSite\nmetadata: {name: alone}\nspec: {host: alone.example.com, port: 9090, proxy: 'not-a-url'}\n",
        );
        let (cfg, reports) =
            conduit_k8s::SiteSet::<ConduitSchema>::new().build(all.iter(), "127.0.0.1:2019");
        // `bad` shares port 8080 with `other`: a placeholder answers for it. `alone` has a port of its own: left out.
        assert_eq!(cfg.sites.len(), 2);
        // The first pass of `find_site_idx` is an exact host match on the port, so this site is the one chosen.
        let site = cfg
            .sites
            .iter()
            .find(|s| s.host.as_deref() == Some("bad.example.com") && s.port == Some(8080))
            .expect("a site answers for the host");
        assert!(site.proxy.is_none(), "the placeholder proxies nothing");
        assert_eq!(site.fallback.as_ref().and_then(|f| f.status), Some(503));
        assert!(reports[1].message.contains("503"), "{}", reports[1].message);
        assert!(
            !reports[2].message.contains("503"),
            "{}",
            reports[2].message
        );
        // a conflict-free config: the placeholder passes validation next to the others
        assert!(
            crate::config::validate::partition_by_severity(crate::config::validate::validate(&cfg))
                .1
                .is_empty()
        );
    }

    /// The placeholder of a dropped site must never make the whole config invalid, or one bad object would hold back
    /// every other site again: no placeholder on a `tcp` port, next to a site with its host, or twice for one host;
    /// and a dropped catch-all is answered too.
    #[test]
    fn a_placeholder_never_makes_the_whole_config_invalid() {
        let hdr = "apiVersion: conduit.lopatnov.github.io/v1\nkind: ConduitSite\n";
        let yaml = format!(
            "{hdr}metadata: {{name: web}}\nspec: {{host: web.example.com, port: 8080, proxy: 'http://a:1'}}\n---\n\
             {hdr}metadata: {{name: db}}\nspec: {{port: 5432, tcp: {{targets: ['db:5432']}}}}\n---\n\
             {hdr}metadata: {{name: copy-of-web}}\nspec: {{host: WEB.example.com, port: 8080, proxy: 'not-a-url'}}\n---\n\
             {hdr}metadata: {{name: on-the-tcp-port}}\nspec: {{host: x.example.com, port: 5432, proxy: 'not-a-url'}}\n---\n\
             {hdr}metadata: {{name: bad-1}}\nspec: {{host: twice.example.com, port: 8080, proxy: 'not-a-url'}}\n---\n\
             {hdr}metadata: {{name: bad-2}}\nspec: {{host: twice.example.com, port: 8080, proxy: 'not-a-url'}}\n---\n\
             {hdr}metadata: {{name: bad-catch-all}}\nspec: {{port: 8080, proxy: 'not-a-url'}}\n"
        );
        let all = crds_in(&yaml);
        let (cfg, reports) =
            conduit_k8s::SiteSet::<ConduitSchema>::new().build(all.iter(), "127.0.0.1:2019");
        let (_, hard) =
            crate::config::validate::partition_by_severity(crate::config::validate::validate(&cfg));
        assert!(hard.is_empty(), "{hard:?}");
        let answered: Vec<&str> = reports
            .iter()
            .filter(|r| r.message.contains("503"))
            .map(|r| r.name.as_str())
            .collect();
        // one of the two bad objects of one host, and the catch-all; nothing for the copy or the tcp port
        assert_eq!(answered, ["bad-1", "bad-catch-all"]);
        // the catch-all has no host, so an unknown host on 8080 reaches it and not the first site
        assert!(cfg.sites.iter().any(|s| s.host.is_none()
            && s.port == Some(8080)
            && s.fallback.as_ref().and_then(|f| f.status) == Some(503)));
    }

    // ── build_app_config ──────────────────────────────────────────────────────

    #[test]
    fn build_app_config_empty_produces_empty_sites() {
        let cfg =
            build_app_config(std::iter::empty(), "127.0.0.1:2019").expect("build must succeed");
        assert!(cfg.sites.is_empty());
        assert!(cfg.global.is_some());
    }

    #[test]
    fn build_app_config_multiple_sites() {
        let sites = [
            make_crd("site-a", make_spec(8080, Some("a.example.com"))),
            make_crd("site-b", make_spec(8081, Some("b.example.com"))),
        ];
        let cfg = build_app_config(sites.iter(), "127.0.0.1:2019").expect("build must succeed");
        assert_eq!(cfg.sites.len(), 2);
        assert_eq!(cfg.sites[0].port, Some(8080));
        assert_eq!(cfg.sites[1].port, Some(8081));
    }

    #[test]
    fn build_app_config_sets_admin_bind() {
        let cfg = build_app_config(std::iter::empty(), "0.0.0.0:9090").expect("build must succeed");
        let bind = cfg
            .global
            .as_ref()
            .and_then(|g| g.admin.as_ref())
            .and_then(|a| a.bind.as_deref());
        assert_eq!(bind, Some("0.0.0.0:9090"));
    }

    // ── KubernetesProvider ────────────────────────────────────────────────────

    #[test]
    fn kubernetes_provider_name_is_kubernetes() {
        use conduit_config_core::provider::Provider as _;
        assert_eq!(KubernetesProvider::new("default").name(), "kubernetes");
    }

    #[test]
    fn kubernetes_provider_stores_namespace() {
        let p = KubernetesProvider::new("my-ns");
        assert_eq!(p.namespace, "my-ns");
    }

    #[test]
    fn kubernetes_provider_with_admin_bind() {
        let p = KubernetesProvider::new("default").with_admin_bind("0.0.0.0:2019");
        assert_eq!(p.admin_bind, "0.0.0.0:2019");
    }
}
