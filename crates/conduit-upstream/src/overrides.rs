//! Runtime upstream overrides of the [`UpstreamRegistry`] (`POST /upstreams/add|remove|weight`).
//!
//! A second `impl UpstreamRegistry` block, split out of `health.rs` (issue #633).

use crate::health::UpstreamRegistry;

// ── Override key helpers ──────────────────────────────────────────────────────

/// Build the DashMap key for a runtime override entry.
///
/// Format: `"{site}\0{route}"`.  The null byte is a safe separator because
/// neither host labels nor URL paths can contain `\0`.
/// Site `"*"` is the wildcard — applies to every site that serves the route.
pub(crate) fn override_key(site: &str, route: &str) -> String {
    format!("{site}\0{route}")
}

impl UpstreamRegistry {
    // ── Runtime upstream overrides (Phase 2.5c) ───────────────────────────────

    /// Return the runtime override target list for `(site_label, route)`.
    ///
    /// Lookup order:
    /// 1. Site-specific key — `"{site_label}\0{route}"` (set via `--site` flag).
    /// 2. Wildcard key — `"*\0{route}"` (set when no `--site` is specified).
    /// 3. `None` — fall back to the config-file targets.
    pub fn get_override_targets(
        &self,
        site_label: &str,
        route: &str,
    ) -> Option<Vec<(String, u32)>> {
        // Site-specific takes precedence.
        let specific = override_key(site_label, route);
        if let Some(v) = self.overrides.get(&specific) {
            return Some(v.value().clone());
        }
        // Wildcard — applies to all sites.
        self.overrides
            .get(&override_key("*", route))
            .map(|e| e.value().clone())
    }

    /// Add `url` with `weight` to the runtime override list for `(site, route)`.
    ///
    /// `site` is either a site label (e.g. `"app.example.com:443"`) for a
    /// site-specific override, or `"*"` to apply the override to every site
    /// that serves this route (wildcard, backward-compatible default).
    ///
    /// If `url` is already present its weight is updated.
    pub fn add_upstream(&self, site: &str, route: &str, url: &str, weight: u32) {
        let key = override_key(site, route);
        let mut entry = self.overrides.entry(key).or_default();
        if let Some(existing) = entry.iter_mut().find(|(u, _)| u == url) {
            existing.1 = weight;
        } else {
            entry.push((url.to_owned(), weight));
        }
    }

    /// Remove `url` from the runtime override list for `(site, route)`.
    ///
    /// Returns `true` if the URL was found and removed.
    pub fn remove_upstream(&self, site: &str, route: &str, url: &str) -> bool {
        let key = override_key(site, route);
        if let Some(mut entry) = self.overrides.get_mut(&key) {
            let before = entry.len();
            entry.retain(|(u, _)| u != url);
            return entry.len() < before;
        }
        false
    }

    /// Update the weight of `url` within the runtime override list for `(site, route)`.
    ///
    /// Returns `true` when the URL was found and its weight updated.
    pub fn set_weight(&self, site: &str, route: &str, url: &str, weight: u32) -> bool {
        let key = override_key(site, route);
        if let Some(mut entry) = self.overrides.get_mut(&key) {
            if let Some(existing) = entry.iter_mut().find(|(u, _)| u == url) {
                existing.1 = weight;
                return true;
            }
        }
        false
    }

    /// Drop all runtime overrides (called on `POST /reload`).
    pub fn clear_overrides(&self) {
        self.overrides.clear();
    }
}
