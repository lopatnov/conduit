//! The feature-off warnings for `proxy` and `routes[].proxy`, called by the root crate's `config::validate::feature_warnings()`.

use crate::config::{ProxyConfig, RouteConfig};

/// `true` when this build has the `proxy` feature. The root crate asserts at compile time that its own `proxy` feature
/// agrees (`src/config/validate/warnings.rs`), so the two cannot drift apart unnoticed.
pub const COMPILED: bool = cfg!(feature = "proxy");

/// Warn about `proxy` configuration a build without the `proxy` feature cannot honour (issue #144): the router ignores
/// `sites[].proxy` entirely and ends a `routes[]` entry with a `proxy` action in the site's fallback response, so without
/// this the operator would just see 404s and no explanation. The site-level warning comes first, then one per route, in
/// route order. `i` is the site index used in the messages.
pub fn feature_warnings(
    i: usize,
    proxy: Option<&ProxyConfig>,
    routes: Option<&[RouteConfig]>,
    out: &mut Vec<String>,
) {
    if COMPILED {
        return;
    }
    if proxy.is_some() {
        out.push(proxy_feature_warning_text(i));
    }
    for (j, route) in routes.into_iter().flatten().enumerate() {
        if route.proxy.is_some() {
            out.push(route_proxy_feature_warning_text(i, j));
        }
    }
}

fn proxy_feature_warning_text(i: usize) -> String {
    format!(
        "sites[{i}].proxy is configured but Conduit was compiled without the `proxy` \
         feature — reverse proxying is disabled and this configuration will be ignored \
         (requests fall through to `static`/`fallback`). \
         Recompile with `--features proxy` to enable."
    )
}

fn route_proxy_feature_warning_text(i: usize, j: usize) -> String {
    format!(
        "sites[{i}].routes[{j}].proxy is configured but Conduit was compiled without \
         the `proxy` feature — requests matching this route are answered by the \
         site's `fallback` response (its `static` action, if any, is not served \
         either). Recompile with `--features proxy` to enable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyRouteTarget;

    #[test]
    fn texts_are_pinned() {
        assert_eq!(
            proxy_feature_warning_text(3),
            "sites[3].proxy is configured but Conduit was compiled without the `proxy` \
             feature — reverse proxying is disabled and this configuration will be \
             ignored (requests fall through to `static`/`fallback`). Recompile with \
             `--features proxy` to enable."
        );
        assert_eq!(
            route_proxy_feature_warning_text(3, 5),
            "sites[3].routes[5].proxy is configured but Conduit was compiled without the \
             `proxy` feature — requests matching this route are answered by the site's \
             `fallback` response (its `static` action, if any, is not served either). \
             Recompile with `--features proxy` to enable."
        );
    }

    #[test]
    fn nothing_configured_nothing_warned() {
        let mut out = Vec::new();
        feature_warnings(3, None, None, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn site_proxy_then_each_route_proxy_in_order() {
        let proxy = ProxyConfig::Single("http://a".to_owned());
        let with_proxy = RouteConfig {
            proxy: Some(ProxyRouteTarget::Url("http://b".to_owned())),
            ..RouteConfig::default()
        };
        let routes = [RouteConfig::default(), with_proxy];
        let mut out = Vec::new();
        feature_warnings(3, Some(&proxy), Some(&routes), &mut out);
        let expected = if COMPILED {
            Vec::new()
        } else {
            vec![
                proxy_feature_warning_text(3),
                route_proxy_feature_warning_text(3, 1),
            ]
        };
        assert_eq!(out, expected);
    }
}
