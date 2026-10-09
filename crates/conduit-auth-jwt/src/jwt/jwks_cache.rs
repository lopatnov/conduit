//! The JWKS key cache behind `jwtAuth.jwksUrl` (issue #163).
//!
//! Keys are fetched lazily on the first request that needs them and refreshed once they are older than
//! `jwksRefreshSecs`. The guard that reads them is synchronous and runs on a Pingora worker thread, so the cache is
//! built to keep that thread off the network whenever it can:
//!
//! - **Single-flight.** At most one fetch per URL is in flight. A cold cache blocks the first requests on one shared
//!   fetch instead of letting each start its own; a stale cache refreshes on one background thread.
//! - **Stale-while-revalidate.** Once keys exist, a request never waits for the network: past `jwksRefreshSecs` it
//!   keeps verifying against the last good key set while the background refresh runs.
//! - **Stale fallback.** When a refresh fails (IdP outage), the last good keys keep serving for up to
//!   [`MAX_STALE`] beyond the refresh interval, and a failed fetch is not retried for [`RETRY_BACKOFF`], so an
//!   outage does not turn every request into a 401 or hammer the IdP. Past that window the keys are no longer
//!   trusted (a rotated-out or revoked key must not verify forever) and requests fail closed until a fetch succeeds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// How long past `jwksRefreshSecs` the last good key set may still verify tokens when refreshing keeps failing.
pub(super) const MAX_STALE: Duration = Duration::from_secs(24 * 60 * 60);
/// After a failed fetch, no new fetch for this long (the last good keys, if any, keep serving meanwhile).
pub(super) const RETRY_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(super) enum CachedKey {
    Rsa {
        n: String,
        e: String,
    },
    Ec {
        x: String,
        y: String,
        // Parsed from the JWK for completeness but not currently consulted —
        // `DecodingKey::from_ec_components` infers the curve from the JWT's
        // `alg` header (ES256 → P-256, ES384 → P-384), not from this field.
        #[allow(dead_code)]
        crv: String,
    },
}

/// Map from `kid` → public key material.
pub(super) type KeyMap = HashMap<String, CachedKey>;

struct Snapshot {
    keys: Arc<KeyMap>,
    fetched_at: Instant,
}

/// Everything the cache knows about one JWKS URL.
struct UrlState {
    cached: RwLock<Option<Snapshot>>,
    /// Set by the one thread that owns the background refresh.
    refreshing: AtomicBool,
    /// Held by the one thread that is fetching while nothing usable is cached (cold start, or keys past `MAX_STALE`).
    blocking_fetch: Mutex<()>,
    last_failure: Mutex<Option<Instant>>,
}

impl UrlState {
    fn new() -> Self {
        Self {
            cached: RwLock::new(None),
            refreshing: AtomicBool::new(false),
            blocking_fetch: Mutex::new(()),
            last_failure: Mutex::new(None),
        }
    }

    /// The cached keys and their age, if any.
    fn snapshot(&self) -> Option<(Arc<KeyMap>, Duration)> {
        let cached = self.cached.read().unwrap_or_else(PoisonError::into_inner);
        cached
            .as_ref()
            .map(|s| (Arc::clone(&s.keys), s.fetched_at.elapsed()))
    }

    fn in_backoff(&self) -> bool {
        lock(&self.last_failure).is_some_and(|at| at.elapsed() < RETRY_BACKOFF)
    }

    /// Record the outcome of a fetch and return the new key set on success.
    fn finish_fetch(&self, url: &str, result: anyhow::Result<KeyMap>) -> Option<Arc<KeyMap>> {
        match result {
            Ok(keys) => {
                let keys = Arc::new(keys);
                *self.cached.write().unwrap_or_else(PoisonError::into_inner) = Some(Snapshot {
                    keys: Arc::clone(&keys),
                    fetched_at: Instant::now(),
                });
                *lock(&self.last_failure) = None;
                Some(keys)
            }
            Err(e) => {
                tracing::warn!(
                    jwks_url_host = %host_of(url),
                    "JWKS fetch failed: {e:#} — keeping the last good keys, if any, and retrying in {}s",
                    RETRY_BACKOFF.as_secs()
                );
                *lock(&self.last_failure) = Some(Instant::now());
                None
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The host part of `url`, for logs (the rest of a JWKS URL may carry a secret).
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.rsplit('@').next().unwrap_or(authority)
}

/// Global per-URL state.
static STATES: OnceLock<RwLock<HashMap<String, Arc<UrlState>>>> = OnceLock::new();

fn state_for(url: &str) -> Arc<UrlState> {
    let states = STATES.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(s) = states
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(url)
    {
        return Arc::clone(s);
    }
    let mut w = states.write().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(
        w.entry(url.to_owned())
            .or_insert_with(|| Arc::new(UrlState::new())),
    )
}

// ── Minimal JWKS JSON types ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    #[serde(rename = "kty")]
    key_type: String,
    #[serde(default)]
    kid: Option<String>,
    // RSA
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    // EC
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default, rename = "crv")]
    curve: Option<String>,
}

// ── JWKS fetch ────────────────────────────────────────────────────────────────

/// Fetch and parse the JWKS document at `url`. Errors never carry the URL (it may hold a secret).
pub(super) async fn fetch_jwks(url: &str) -> anyhow::Result<KeyMap> {
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(reqwest::Error::without_url)?
        .get(url)
        .send()
        .await
        .map_err(reqwest::Error::without_url)?
        .error_for_status()
        .map_err(reqwest::Error::without_url)?
        .json::<JwksResponse>()
        .await
        .map_err(reqwest::Error::without_url)?;

    let mut map = HashMap::new();
    for jwk in resp.keys {
        let kid = jwk
            .kid
            .unwrap_or_else(|| format!("{}-default", jwk.key_type));
        let cached = match jwk.key_type.as_str() {
            "RSA" => {
                if let (Some(n), Some(e)) = (jwk.n, jwk.e) {
                    Some(CachedKey::Rsa { n, e })
                } else {
                    tracing::warn!(kid, "JWKS RSA key missing n or e — skipped");
                    None
                }
            }
            "EC" => {
                if let (Some(x), Some(y), Some(crv)) = (jwk.x, jwk.y, jwk.curve) {
                    Some(CachedKey::Ec { x, y, crv })
                } else {
                    tracing::warn!(kid, "JWKS EC key missing x, y, or crv — skipped");
                    None
                }
            }
            other => {
                tracing::debug!(key_type = other, "JWKS key type not supported — skipped");
                None
            }
        };
        if let Some(c) = cached {
            map.insert(kid, c);
        }
    }
    Ok(map)
}

/// Run [`fetch_jwks`] to completion on a throwaway `current_thread` runtime (same pattern as ACME), on its own
/// thread so it works whether or not the caller is inside a Tokio runtime.
fn fetch_blocking(url: &str) -> anyhow::Result<KeyMap> {
    let url = url.to_owned();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(fetch_jwks(&url))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("JWKS fetch thread panicked"))?
}

/// Clears `refreshing` when the background refresh ends, however it ends.
struct RefreshGuard(Arc<UrlState>);

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        self.0.refreshing.store(false, Ordering::Release);
    }
}

/// Start the one background refresh for `url`, unless one is already running or a fetch just failed.
fn spawn_refresh(url: &str, state: &Arc<UrlState>) {
    if state.in_backoff()
        || state
            .refreshing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }
    let url = url.to_owned();
    let guard = RefreshGuard(Arc::clone(state));
    let spawned = std::thread::Builder::new()
        .name("jwks-refresh".to_owned())
        .spawn(move || {
            let result = fetch_blocking(&url);
            guard.0.finish_fetch(&url, result);
        });
    if let Err(e) = spawned {
        // The closure (and its guard) was dropped, so `refreshing` is already cleared.
        tracing::warn!("could not start the JWKS refresh thread: {e}");
    }
}

/// The keys to verify with for `url`, fetching or refreshing as needed (see the module docs).
///
/// `refresh_secs` is the age after which a refresh is started. `None` means no usable keys: the fetch failed (or was
/// recently attempted and failed) and nothing within [`MAX_STALE`] is cached.
pub(super) fn get_jwks_keys(url: &str, refresh_secs: u64) -> Option<Arc<KeyMap>> {
    let refresh = Duration::from_secs(refresh_secs);
    let state = state_for(url);

    if let Some((keys, age)) = state.snapshot() {
        if age < refresh {
            return Some(keys);
        }
        // Stale but still within MAX_STALE: refresh in the background, and keep serving the last good keys while
        // that runs or fails. Past MAX_STALE the blocking single-flight path below does the fetch (no second one).
        if age < refresh.saturating_add(MAX_STALE) {
            spawn_refresh(url, &state);
            return Some(keys);
        }
    }

    // Nothing usable cached (cold start, or past MAX_STALE): one thread fetches, the others wait for its result.
    let _fetching = lock(&state.blocking_fetch);
    if let Some((keys, age)) = state.snapshot() {
        if age < refresh.saturating_add(MAX_STALE) {
            return Some(keys);
        }
    }
    if state.in_backoff() {
        return None;
    }
    let result = fetch_blocking(url);
    state.finish_fetch(url, result)
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// Make the cached keys for `url` look `age` old.
    pub(in crate::jwt) fn age_cache(url: &str, age: Duration) {
        let state = state_for(url);
        let mut cached = state.cached.write().unwrap();
        let snap = cached.as_mut().expect("nothing cached for this URL");
        snap.fetched_at = Instant::now() - age;
    }

    /// Forget a recorded fetch failure for `url`, so the next fetch is attempted immediately.
    pub(in crate::jwt) fn clear_backoff(url: &str) {
        *lock(&state_for(url).last_failure) = None;
    }

    pub(in crate::jwt) fn is_refreshing(url: &str) -> bool {
        state_for(url).refreshing.load(Ordering::Acquire)
    }
}
