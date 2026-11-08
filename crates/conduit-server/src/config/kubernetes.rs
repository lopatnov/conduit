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
//! apiVersion: conduit.io/v1
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
        Ok(site)
    }

    fn build_config(sites: Vec<SiteConfig>, admin_bind: &str) -> AppConfig {
        AppConfig {
            global: Some(GlobalConfig {
                admin: Some(AdminConfig {
                    bind: Some(admin_bind.to_owned()),
                    token: None,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            sites,
            ..Default::default()
        }
    }
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
