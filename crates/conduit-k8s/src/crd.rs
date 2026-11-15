//! `ConduitSite` Custom Resource Definition types.
//!
//! These are schema-agnostic: every field is `Option<serde_json::Value>`
//! mirroring conduit's own `SiteConfig` field names in prose doc comments
//! only — this crate never names the real `SiteConfig`/`AppConfig` types
//! (see the crate-level doc comment in `lib.rs`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use kube::CustomResource;

/// Spec of a `ConduitSite` Kubernetes custom resource.
///
/// Field names match the Conduit JSON config schema so that the spec can be
/// round-tripped through `serde_json` into a `SiteConfig` by the root crate's
/// `CrdConfigBuilder::site_from_spec` implementation.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "conduit.lopatnov.github.io",
    version = "v1",
    kind = "ConduitSite",
    namespaced,
    status = "ConduitSiteStatus",
    shortname = "cs",
    printcolumn = r#"{"name":"Port","type":"integer","jsonPath":".spec.port"}"#,
    printcolumn = r#"{"name":"Host","type":"string","jsonPath":".spec.host"}"#
)]
pub struct ConduitSiteSpec {
    /// TCP port to listen on. Default: 80 (HTTP) or 443 (HTTPS).
    pub port: Option<u16>,

    /// Virtual host for request matching. Omit for catch-all.
    pub host: Option<String>,

    /// Upstream proxy target(s). Accepts the same values as `proxy` in
    /// conduit.json: a URL string, array of URLs, or a route map object.
    pub proxy: Option<serde_json::Value>,

    /// Static file directory path (equivalent to `static` in conduit.json).
    #[serde(rename = "static")]
    pub static_files: Option<serde_json::Value>,

    /// Enable the `/__health__` health-check endpoint.
    #[serde(rename = "healthCheck")]
    pub health_check: Option<serde_json::Value>,

    /// TLS configuration (cert, key, acme, httpRedirectPort).
    pub tls: Option<serde_json::Value>,

    /// Custom response headers added to every response.
    pub headers: Option<serde_json::Value>,

    /// Fallback response when no route matches.
    pub fallback: Option<serde_json::Value>,

    /// Access logging configuration.
    pub logging: Option<serde_json::Value>,

    /// Response compression.
    pub compression: Option<serde_json::Value>,

    /// Token-bucket rate limiting.
    #[serde(rename = "rateLimit")]
    pub rate_limit: Option<serde_json::Value>,

    /// HTTP Basic authentication.
    #[serde(rename = "basicAuth")]
    pub basic_auth: Option<serde_json::Value>,

    /// API-key authentication.
    #[serde(rename = "apiKey")]
    pub api_key: Option<serde_json::Value>,

    /// IP allow/deny filter.
    #[serde(rename = "ipFilter")]
    pub ip_filter: Option<serde_json::Value>,

    /// CORS configuration.
    pub cors: Option<serde_json::Value>,

    /// Prometheus metrics endpoint.
    pub metrics: Option<serde_json::Value>,

    /// File upload endpoint.
    pub upload: Option<serde_json::Value>,

    /// Redirect rules.
    pub redirects: Option<serde_json::Value>,

    /// Rhai scripting middleware.
    pub middleware: Option<serde_json::Value>,

    /// Advanced routes array.
    pub routes: Option<serde_json::Value>,

    /// Every other site field (`jwtAuth`, `consumers`, `forwardAuth`, `securityHeaders`,
    /// `limits`, `tcp`, ...). They are kept as written, so that a `ConduitSite` accepts every
    /// field a site of a config file accepts; the root crate reads them into its own site type.
    /// A key that no site field matches is reported as a warning there, as in a config file.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Status subresource written back by the Conduit controller.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Default)]
pub struct ConduitSiteStatus {
    /// Human-readable summary (e.g. "Listening on :8080").
    pub message: Option<String>,
    /// Whether this site is currently active.
    pub ready: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::ConduitSite;
    use kube::Resource;

    /// The CRD manifest in `contrib/k8s` is written by hand, and the controller watches the group
    /// named by the derive above. A mismatch makes the controller see no objects and say nothing.
    #[test]
    fn the_crd_manifest_names_the_group_the_controller_watches() {
        let group = ConduitSite::group(&());
        let manifest = include_str!("../../../contrib/k8s/conduitsite-crd.yaml");
        assert!(
            manifest
                .lines()
                .any(|l| l.trim() == format!("group: {group}")),
            "contrib/k8s/conduitsite-crd.yaml must declare `group: {group}`"
        );
        let example = include_str!("../../../contrib/k8s/example-site.yaml");
        assert!(
            example.contains(&format!("apiVersion: {group}/v1")),
            "contrib/k8s/example-site.yaml must use `apiVersion: {group}/v1`"
        );
        // A typo in either of these is silent: the CRD is named `<plural>.<group>`, and a Role that
        // names another group is a 403 on the first list.
        assert!(
            manifest.contains(&format!("name: conduitsites.{group}")),
            "the CRD must be named `conduitsites.{group}`"
        );
        assert!(
            example.contains(&format!("apiGroups: [{group}]")),
            "the Role in example-site.yaml must grant `apiGroups: [{group}]`"
        );
    }
}
