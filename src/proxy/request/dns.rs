//! Hostname resolution cache (issue #232) used by [`super::peer`]'s
//! `upstream_peer`/`resolve_peer_addr`.
//!
//! Split out of the former monolithic `request_phase.rs` (issue #144 prep,
//! PR 1 of 2) -- pure code relocation, no behavioral change.

use std::net::SocketAddr;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use dashmap::DashMap;

/// In-memory cache of resolved hostname → candidate [`SocketAddr`]s, keyed
/// by the exact `host:port` string passed to [`resolve_socket_addr`].
///
/// Issue #232: without this, every proxied request to a hostname-based
/// upstream (Docker/k8s service names, `localhost`, etc.) paid a blocking-
/// pool `getaddrinfo` call on every single request and again on every
/// retry — no caching layer at all. IP-literal upstreams (the common
/// load-balancer case) already take the fast synchronous path below and
/// never touch this cache.
///
/// **Cache key space is bounded by configured upstream endpoints, not by
/// client input**: `addr_str` always originates from `sites[].proxy` /
/// `routes[]` / retry-URL config (see `resolve_peer_addr`), never from a
/// client-supplied header or path — a client cannot grow this map *within a
/// single running config*. Across many `conduit reload`s over a
/// long-running process's lifetime, though, the set of *ever*-configured
/// hostnames only grows (Gitar/CodeRabbit findings on this PR: an entry
/// whose hostname later drops out of config, or whose TTL simply expires,
/// stops being *served* but was never actually removed) — [`dns_cache_store`]
/// sweeps every expired entry out once the map crosses
/// [`DNS_CACHE_SWEEP_THRESHOLD`], so long-running memory use stays bounded
/// near that threshold rather than growing forever.
static DNS_CACHE: std::sync::OnceLock<DashMap<String, DnsCacheEntry>> = std::sync::OnceLock::new();

fn dns_cache() -> &'static DashMap<String, DnsCacheEntry> {
    DNS_CACHE.get_or_init(DashMap::new)
}

struct DnsCacheEntry {
    /// Every resolved address of the preferred family (see
    /// [`filter_preferred_family`]) — never empty, since
    /// [`dns_cache_store`] is only ever called with a non-empty list.
    addrs: Vec<SocketAddr>,
    resolved_at: Instant,
    /// Round-robin cursor, advanced (mod `addrs.len()`) on every pick —
    /// including the very first one, at resolution time — so repeated
    /// cache hits for a multi-A-record hostname (e.g. a Kubernetes
    /// headless service, or round-robin DNS used as a poor-man's load
    /// balancer) rotate through every resolved address instead of pinning
    /// all traffic onto whichever one happened to be picked first for the
    /// full TTL window (Gitar finding on this PR).
    next: std::sync::atomic::AtomicUsize,
}

/// How long a resolved hostname→addresses mapping stays valid before the
/// next lookup for that hostname triggers a fresh DNS resolution.
///
/// Deliberately short and not (yet) exposed as config: this is a pure
/// perf/scalability fix, not a correctness knob a deployment would need to
/// tune (unlike `jwksRefreshSecs`/`cache.earlyRefreshSecs`, which gate real
/// security/freshness tradeoffs). 30s also answers the "hot-reload staleness"
/// question the issue raised: an upstream URL changed via `conduit reload`
/// is served from a stale cached address for at most one TTL window — short
/// enough that no explicit reload-triggered cache invalidation is needed,
/// the cache self-heals regardless of when a reload happens to land.
const DNS_CACHE_TTL_SECS: u64 = 30;

/// Once [`dns_cache`]'s entry count reaches this, the next
/// [`dns_cache_store`] call sweeps out every already-expired entry before
/// inserting (rate-limited to once per [`DNS_CACHE_TTL_SECS`] window by
/// [`sweep_due`] — see its doc comment). Bounds long-running memory growth
/// (CodeRabbit finding on this PR) without needing to hook cache pruning
/// into the hot-reload path: a reload that stops using some hostname just
/// leaves its entry to expire and get swept on the next due store past this
/// threshold, rather than living forever. Picked well above any realistic
/// number of distinct hostname upstreams a single instance would proxy to,
/// so the sweep is rare in normal operation and only actually engages for
/// pathological long-running + high-hostname-churn deployments.
const DNS_CACHE_SWEEP_THRESHOLD: usize = 512;

/// Timestamp of the last completed sweep, gating how often [`dns_cache_store`]
/// is willing to run one.
static LAST_SWEEP: std::sync::OnceLock<std::sync::Mutex<Option<Instant>>> =
    std::sync::OnceLock::new();

/// Whether enough time has passed since the last sweep to run another one —
/// at most once per [`DNS_CACHE_TTL_SECS`] window.
///
/// Gitar finding on this PR's first sweep implementation: gating purely on
/// `cache.len() >= DNS_CACHE_SWEEP_THRESHOLD` means that once a deployment
/// has more than [`DNS_CACHE_SWEEP_THRESHOLD`] *concurrently-live* (still
/// within TTL) hostname upstreams, `cache.retain(...)` removes nothing but
/// still pays a full O(n) scan on every single cache-miss store thereafter.
/// Rate-limiting to once per TTL window means a saturated-but-live cache
/// pays for at most one no-op scan per window instead of one per miss —
/// entries only start expiring at the same TTL cadence this gate uses, so a
/// sweep more often than that could never find anything new to remove
/// anyway.
fn sweep_due() -> bool {
    let mut last = LAST_SWEEP
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap();
    let now = Instant::now();
    let due = last.is_none_or(|t| now.duration_since(t).as_secs() >= DNS_CACHE_TTL_SECS);
    if due {
        *last = Some(now);
    }
    due
}

fn round_robin_pick(entry: &DnsCacheEntry) -> SocketAddr {
    let idx = entry
        .next
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        % entry.addrs.len();
    entry.addrs[idx]
}

fn dns_cache_lookup(key: &str) -> Option<SocketAddr> {
    let entry = dns_cache().get(key)?;
    (entry.resolved_at.elapsed().as_secs() < DNS_CACHE_TTL_SECS).then(|| round_robin_pick(&entry))
}

/// Stores a freshly-resolved address list and returns the first pick from
/// it (round-robin cursor starts at 0), so callers don't need a separate
/// lookup immediately after storing.
fn dns_cache_store(key: String, addrs: Vec<SocketAddr>) -> SocketAddr {
    debug_assert!(
        !addrs.is_empty(),
        "dns_cache_store must not be called with an empty address list"
    );
    let cache = dns_cache();
    if cache.len() >= DNS_CACHE_SWEEP_THRESHOLD && sweep_due() {
        cache.retain(|_, entry| entry.resolved_at.elapsed().as_secs() < DNS_CACHE_TTL_SECS);
    }
    let entry = DnsCacheEntry {
        addrs,
        resolved_at: Instant::now(),
        next: std::sync::atomic::AtomicUsize::new(0),
    };
    let picked = round_robin_pick(&entry);
    cache.insert(key, entry);
    picked
}

/// Abstraction over "resolve a hostname to a list of candidate addresses",
/// so the DNS cache above can get genuine end-to-end test coverage — a fake
/// resolver whose answers change *between* calls proves a cache hit really
/// does skip re-resolution (the caller gets the stale answer until the TTL
/// expires, then the fresh one), which two real calls to `localhost` can
/// never prove since the OS resolver's answer never changes. This avoids
/// needing to touch system-level DNS config (`/etc/hosts`, `resolv.conf`,
/// or a container) from a test process — those are system-settings changes
/// this session won't make even on request.
#[async_trait]
trait HostResolver: Send + Sync {
    async fn lookup(&self, addr_str: &str) -> std::io::Result<Vec<SocketAddr>>;
}

/// Production resolver: delegates to the OS resolver via
/// [`tokio::net::lookup_host`] — identical behavior to before this
/// abstraction existed, just behind the trait so tests can substitute
/// [`FakeHostResolver`] instead.
struct TokioHostResolver;

#[async_trait]
impl HostResolver for TokioHostResolver {
    async fn lookup(&self, addr_str: &str) -> std::io::Result<Vec<SocketAddr>> {
        Ok(tokio::net::lookup_host(addr_str).await?.collect())
    }
}

/// Resolve a `host:port` string to a [`SocketAddr`], accepting both IP
/// literals and hostnames.
///
/// IP literals take a fast synchronous path (no behavior change from before
/// hostname support was added). Hostnames go through async DNS resolution
/// via the supplied [`HostResolver`] (production callers always use
/// [`TokioHostResolver`] — see [`resolve_socket_addr`]), short-circuited by
/// [`DNS_CACHE`] when a fresh-enough entry already exists. Resolving via a
/// trait rather than calling `tokio::net::lookup_host` directly (or relying
/// on `HttpPeer::new`'s own resolution — that constructor takes
/// `impl std::net::ToSocketAddrs`, which resolves *synchronously* (blocking
/// the async runtime thread) and `.unwrap()`s the result, unacceptable
/// inside a per-request async hook) is what makes the cache's actual
/// caching behavior (not just its output) testable.
///
/// `resolution_timeout` bounds the DNS lookup with the same effective
/// connect deadline `apply_peer_options` derives for the connection itself
/// (`proxy.*.timeout.connectMs`, falling back to `limits.timeoutSecs`) —
/// without it, a stalled resolver could hold a request open indefinitely
/// with no deadline at all (CodeRabbit finding on PR #227). `None` (no
/// configured timeout at all) resolves without a deadline, matching
/// `apply_peer_options`'s own "absent config = no enforced timeout" default.
/// A cache hit never awaits, so it costs none of this budget regardless —
/// `upstream_peer`'s `remaining_budget()` call already handles that
/// correctly with no special-casing needed, since it subtracts *actual*
/// elapsed resolution time, which stays ~0 on a hit.
///
/// This cache operates purely on the already-selected target's `host:port`
/// string, strictly *after* routing/load-balancing has picked which
/// upstream URL to use — it has no interaction with `IpHash`/
/// `ConsistentHash` forward-probing in `capacity.rs`, which hashes on the
/// pre-resolution config URL, not the resolved IP.
///
/// Caches every resolved address of the preferred family, not just one —
/// see [`DnsCacheEntry`]'s doc comment for why: caching a single picked
/// address for the full TTL would silently defeat DNS-level round-robin
/// for a multi-A-record hostname.
async fn resolve_socket_addr_with(
    resolver: &dyn HostResolver,
    addr_str: &str,
    resolution_timeout: Option<Duration>,
) -> pingora_core::Result<SocketAddr> {
    if let Ok(addr) = addr_str.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Some(cached) = dns_cache_lookup(addr_str) {
        return Ok(cached);
    }
    let lookup = resolver.lookup(addr_str);
    let addrs = match resolution_timeout {
        Some(d) => tokio::time::timeout(d, lookup).await.map_err(|_| {
            pingora_core::Error::explain(
                pingora_core::ErrorType::ConnectTimedout,
                format!("DNS resolution timed out for upstream address {addr_str}"),
            )
        })?,
        None => lookup.await,
    }
    .map_err(|e| {
        pingora_core::Error::explain(
            pingora_core::ErrorType::ConnectProxyFailure,
            format!("DNS resolution failed for upstream address {addr_str}: {e}"),
        )
    })?;
    let preferred = filter_preferred_family(addrs.into_iter());
    if preferred.is_empty() {
        return Err(pingora_core::Error::explain(
            pingora_core::ErrorType::ConnectProxyFailure,
            format!("no addresses found for upstream address {addr_str}"),
        ));
    }
    Ok(dns_cache_store(addr_str.to_string(), preferred))
}

/// Production entry point: resolves via [`TokioHostResolver`] (the real OS
/// resolver). See [`resolve_socket_addr_with`] for the full behavior —
/// tests call that directly with a [`FakeHostResolver`] instead.
pub(super) async fn resolve_socket_addr(
    addr_str: &str,
    resolution_timeout: Option<Duration>,
) -> pingora_core::Result<SocketAddr> {
    resolve_socket_addr_with(&TokioHostResolver, addr_str, resolution_timeout).await
}

/// Filter a hostname's DNS results down to every address of the preferred
/// family, preferring IPv4 — every connection attempt still hands
/// `HttpPeer` exactly one concrete `SocketAddr` (Pingora has no
/// multi-address / Happy-Eyeballs fallback; an unrelated Happy-Eyeballs
/// backlog item is `[🚫 BLOCKED]` in `CLAUDE.md` for the same reason: no
/// public API for parallel connection attempts), but keeping the *whole*
/// preferred-family list — rather than collapsing to one address here —
/// lets [`resolve_socket_addr_with`] cache all of them and round-robin
/// across requests instead of pinning every request onto a single address
/// for the DNS cache's full TTL.
///
/// When a hostname resolves to both families, the OS resolver's ordering
/// is not a reliable signal for which family the upstream actually listens
/// on — glibc's `getaddrinfo` prefers IPv6 by RFC 3484 default regardless
/// of whether the target has a real IPv6 listener. Preferring IPv4
/// deterministically matches the overwhelmingly common case for
/// self-hosted upstreams (Docker service names, `localhost`, bare local
/// dev servers) instead of silently depending on resolver-order luck.
fn filter_preferred_family(addrs: impl Iterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    let addrs: Vec<SocketAddr> = addrs.collect();
    let ipv4: Vec<SocketAddr> = addrs.iter().copied().filter(SocketAddr::is_ipv4).collect();
    if ipv4.is_empty() {
        addrs
    } else {
        ipv4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serial_test::serial;

    // `DNS_CACHE` and the sweep cooldown are process-global, and
    // `dns_cache_store` sweeps once the map reaches `DNS_CACHE_SWEEP_THRESHOLD`.
    // Every test that stores into the cache — directly, or through a
    // successful hostname resolution — therefore carries
    // `#[serial(dns_cache_sweep)]`, otherwise it can land between a sweep
    // test's inserts and its assertions and sweep (or consume the cooldown
    // for) the entries that test depends on (#439). Tests that only read the
    // cache or never reach `dns_cache_store` don't need it.

    // ── resolve_socket_addr (#225: hostname upstreams) ──────────────────────────

    #[tokio::test]
    async fn resolve_socket_addr_ipv4_literal_fast_path() {
        let addr = resolve_socket_addr("127.0.0.1:4000", None).await.unwrap();
        assert_eq!(addr, "127.0.0.1:4000".parse().unwrap());
    }

    #[tokio::test]
    async fn resolve_socket_addr_ipv6_literal_fast_path() {
        let addr = resolve_socket_addr("[::1]:4000", None).await.unwrap();
        assert_eq!(addr, "[::1]:4000".parse().unwrap());
    }

    #[tokio::test]
    #[serial(dns_cache_sweep)]
    async fn resolve_socket_addr_resolves_localhost_hostname() {
        // The exact regression case from #225: "localhost:4000" previously
        // failed SocketAddr::parse and every request to such an upstream
        // would 502. localhost resolves via the OS hosts file/resolver
        // without needing network access, so this is safe to run in CI.
        let addr = resolve_socket_addr("localhost:4000", None).await.unwrap();
        assert!(
            addr.ip().is_loopback(),
            "localhost must resolve to a loopback address, got {addr}"
        );
        assert_eq!(addr.port(), 4000);
    }

    #[tokio::test]
    async fn resolve_socket_addr_unresolvable_hostname_returns_error() {
        let result = resolve_socket_addr("this-host-does-not-exist.invalid:4000", None).await;
        assert!(
            result.is_err(),
            "an unresolvable hostname must return an error, not panic"
        );
    }

    #[tokio::test]
    async fn resolve_socket_addr_ip_literal_ignores_zero_timeout() {
        // The IP-literal fast path never touches the resolver at all, so
        // even a timeout of 0 (which would fire instantly on any real DNS
        // lookup) must not affect it — this is the "no behavior change for
        // the previously-working case" guarantee from the PR description.
        let addr = resolve_socket_addr("127.0.0.1:4000", Some(Duration::ZERO))
            .await
            .unwrap();
        assert_eq!(addr, "127.0.0.1:4000".parse().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn resolve_socket_addr_hostname_respects_timeout() {
        // CodeRabbit finding on PR #227: without a bound, a stalled resolver
        // could hold the request open indefinitely. Racing a zero-duration
        // timeout against a *real* DNS lookup would be flaky — `localhost`
        // often resolves fast enough to win that race on some hosts (this
        // was caught locally: the first version of this test used a real
        // clock and failed non-deterministically). Using a paused virtual
        // clock instead makes this deterministic: `tokio::net::lookup_host`
        // runs on Tokio's blocking-thread pool, so it always returns
        // `Poll::Pending` on its first poll — it cannot complete
        // synchronously within that same poll. The zero-duration deadline
        // has therefore already elapsed on the paused clock by the time
        // `Timeout` checks it, so the timeout branch always wins.
        //
        // Uses a port no other hostname-resolving test in this file uses
        // (#232's DNS_CACHE is a process-global static): if this shared
        // "localhost:4000" key were already cached by
        // `resolve_socket_addr_resolves_localhost_hostname` racing on
        // another thread, this call would hit the cache and return `Ok`
        // instead of exercising the timeout path at all.
        let result = resolve_socket_addr("localhost:4001", Some(Duration::ZERO)).await;
        assert!(
            result.is_err(),
            "a hostname lookup must respect an expired timeout, not block indefinitely"
        );
    }

    // ── DNS resolution cache (#232) ──────────────────────────────────────────

    #[test]
    fn dns_cache_lookup_miss_returns_none_for_unknown_key() {
        assert_eq!(dns_cache_lookup("never-inserted.invalid:9999"), None);
    }

    #[test]
    #[serial(dns_cache_sweep)]
    fn dns_cache_lookup_hit_returns_stored_addr_within_ttl() {
        let key = "cache-hit-test.invalid:4020";
        let addr = v4(4020);
        dns_cache_store(key.to_owned(), vec![addr]);
        assert_eq!(dns_cache_lookup(key), Some(addr));
    }

    #[test]
    fn dns_cache_lookup_expired_entry_returns_none() {
        // Constructs an entry directly (bypassing `dns_cache_store`, which
        // always stamps `Instant::now()`) to simulate one that was resolved
        // longer ago than DNS_CACHE_TTL_SECS, without an actual sleep.
        let key = "cache-expiry-test.invalid:4021";
        let addr = v4(4021);
        dns_cache().insert(
            key.to_owned(),
            DnsCacheEntry {
                addrs: vec![addr],
                resolved_at: Instant::now() - Duration::from_secs(DNS_CACHE_TTL_SECS + 1),
                next: std::sync::atomic::AtomicUsize::new(0),
            },
        );
        assert_eq!(
            dns_cache_lookup(key),
            None,
            "an entry older than the TTL must not be served"
        );
    }

    #[test]
    #[serial(dns_cache_sweep)]
    fn dns_cache_round_robins_across_multiple_addrs() {
        // Foundation of the Gitar-flagged round-robin fix: a multi-address
        // cache entry must rotate through every address on successive
        // lookups, not pin every caller onto the first one.
        let key = "cache-round-robin-test.invalid:4025";
        let a = v4(4025);
        let b = v4(4026);
        let c = v4(4027);
        dns_cache_store(key.to_owned(), vec![a, b, c]);
        // dns_cache_store already consumed index 0 (returned `a`), so the
        // next three lookups continue the rotation and then wrap around.
        assert_eq!(dns_cache_lookup(key), Some(b));
        assert_eq!(dns_cache_lookup(key), Some(c));
        assert_eq!(dns_cache_lookup(key), Some(a));
        assert_eq!(dns_cache_lookup(key), Some(b));
    }

    /// Resets the sweep-rate-limit cooldown ([`sweep_due`]/[`LAST_SWEEP`])
    /// so a sweep-threshold test isn't at the mercy of whichever other
    /// `#[serial(dns_cache_sweep)]` test happened to run immediately before
    /// it in the same process. Test-only: production code never needs to
    /// force a sweep to be due.
    fn reset_sweep_cooldown_for_test() {
        *LAST_SWEEP
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap() = None;
    }

    #[test]
    #[serial(dns_cache_sweep)]
    fn dns_cache_store_sweeps_expired_entries_past_threshold() {
        // Populate the cache with more already-expired entries than
        // DNS_CACHE_SWEEP_THRESHOLD, all under keys unique to this test (a
        // process-global static shared with every other test in this
        // module), then confirm the next `dns_cache_store` call both sweeps
        // them out and successfully inserts the new key.
        //
        // `#[serial(dns_cache_sweep)]` + the cooldown reset below: this test
        // and `dns_cache_sweep_is_rate_limited_within_ttl_window` both
        // depend on the sweep actually running on their first over-threshold
        // store, but they share the same process-global sweep cooldown
        // (`LAST_SWEEP`) — without serializing and resetting it, whichever
        // test happens to run second could find the cooldown already
        // consumed by the other and wrongly see no sweep happen.
        reset_sweep_cooldown_for_test();
        let prefix = "cache-sweep-test.invalid";
        for i in 0..DNS_CACHE_SWEEP_THRESHOLD {
            dns_cache().insert(
                format!("{prefix}:{i}"),
                DnsCacheEntry {
                    addrs: vec![v4(1)],
                    resolved_at: Instant::now() - Duration::from_secs(DNS_CACHE_TTL_SECS + 1),
                    next: std::sync::atomic::AtomicUsize::new(0),
                },
            );
        }
        let before = dns_cache().len();
        assert!(
            before >= DNS_CACHE_SWEEP_THRESHOLD,
            "test setup must actually reach the sweep threshold"
        );

        let new_key = format!("{prefix}:fresh");
        let new_addr = v4(2);
        dns_cache_store(new_key.clone(), vec![new_addr]);

        assert_eq!(
            dns_cache_lookup(&new_key),
            Some(new_addr),
            "the triggering store must still succeed"
        );
        assert!(
            dns_cache().len() < before,
            "crossing the sweep threshold must remove expired entries, not just add to them \
             (before: {before}, after: {})",
            dns_cache().len()
        );
        for i in 0..DNS_CACHE_SWEEP_THRESHOLD {
            assert!(
                dns_cache().get(&format!("{prefix}:{i}")).is_none(),
                "every expired entry from this test must have been swept"
            );
        }
    }

    #[test]
    #[serial(dns_cache_sweep)]
    fn dns_cache_sweep_is_rate_limited_within_ttl_window() {
        // Gitar finding on this PR's sweep: a store past the threshold must
        // not re-scan the whole map on *every* subsequent miss once the
        // cache is saturated with still-live entries — only once per TTL
        // window. See `reset_sweep_cooldown_for_test`'s doc comment for why
        // this shares a `#[serial]` group with the sibling sweep test.
        reset_sweep_cooldown_for_test();
        let prefix = "cache-sweep-rate-limit-test.invalid";

        // First batch past the threshold: the triggering store must sweep
        // (proven in detail by the sibling test above; here just confirmed
        // this batch is actually gone before moving on).
        for i in 0..DNS_CACHE_SWEEP_THRESHOLD {
            dns_cache().insert(
                format!("{prefix}:a:{i}"),
                DnsCacheEntry {
                    addrs: vec![v4(1)],
                    resolved_at: Instant::now() - Duration::from_secs(DNS_CACHE_TTL_SECS + 1),
                    next: std::sync::atomic::AtomicUsize::new(0),
                },
            );
        }
        dns_cache_store(format!("{prefix}:trigger-a"), vec![v4(2)]);
        assert!(
            dns_cache().get(&format!("{prefix}:a:0")).is_none(),
            "the first over-threshold store must sweep"
        );

        // Immediately push a *second* batch of expired entries past the
        // threshold again, well within the same TTL window as the sweep
        // that just ran, then store once more right away.
        for i in 0..DNS_CACHE_SWEEP_THRESHOLD {
            dns_cache().insert(
                format!("{prefix}:b:{i}"),
                DnsCacheEntry {
                    addrs: vec![v4(1)],
                    resolved_at: Instant::now() - Duration::from_secs(DNS_CACHE_TTL_SECS + 1),
                    next: std::sync::atomic::AtomicUsize::new(0),
                },
            );
        }
        let before_second = dns_cache().len();
        assert!(before_second >= DNS_CACHE_SWEEP_THRESHOLD);

        dns_cache_store(format!("{prefix}:trigger-b"), vec![v4(3)]);

        assert_eq!(
            dns_cache().len(),
            before_second + 1,
            "a store within the same TTL window as the last sweep must not sweep again \
             (Gitar finding: unconditional len-based gating would re-scan the whole map on \
             every miss once saturated), it must only insert the new entry"
        );
        assert!(
            dns_cache().get(&format!("{prefix}:b:0")).is_some(),
            "the second expired batch must still be sitting there unswept, proving the \
             rate limit actually suppressed the sweep rather than it just finding nothing to do"
        );
    }

    #[tokio::test]
    #[serial(dns_cache_sweep)]
    async fn resolve_socket_addr_hostname_populates_cache() {
        let key = "localhost:4022";
        let addr = resolve_socket_addr(key, None).await.unwrap();
        assert_eq!(
            dns_cache_lookup(key),
            Some(addr),
            "a successful hostname resolution must populate the cache for reuse"
        );
    }

    #[tokio::test]
    async fn resolve_socket_addr_ip_literal_never_touches_cache() {
        // The IP-literal fast path returns before the cache is consulted at
        // all — issue #232 explicitly calls out that IP-literal upstreams
        // (the common load-balancer case) must stay unaffected.
        let key = "127.0.0.1:4023";
        let _ = resolve_socket_addr(key, None).await.unwrap();
        assert_eq!(
            dns_cache_lookup(key),
            None,
            "the IP-literal fast path must not populate the DNS cache"
        );
    }

    #[tokio::test]
    #[serial(dns_cache_sweep)]
    async fn resolve_socket_addr_second_call_returns_cached_addr() {
        // Integration-style sanity check of the real production entry point
        // (`resolve_socket_addr`, wired to the real `TokioHostResolver`).
        // The stronger proof that caching itself works — not just that two
        // calls happen to agree — is
        // `resolve_socket_addr_with_cache_hit_skips_resolver_call` below,
        // which uses `FakeHostResolver` to change the answer between calls.
        let key = "localhost:4024";
        let first = resolve_socket_addr(key, None).await.unwrap();
        let second = resolve_socket_addr(key, None).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(dns_cache_lookup(key), Some(second));
    }

    /// Fake [`HostResolver`] whose answer for a given key can be changed
    /// between calls, and which counts how many times it was actually
    /// invoked — the two ingredients needed to prove the DNS cache really
    /// short-circuits resolution rather than merely returning a value that
    /// happens to match (which two real `localhost` lookups can never rule
    /// out, since the OS resolver's answer for it never changes).
    struct FakeHostResolver {
        answers: std::sync::Mutex<std::collections::HashMap<String, Vec<SocketAddr>>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeHostResolver {
        fn new() -> Self {
            Self {
                answers: std::sync::Mutex::new(std::collections::HashMap::new()),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn set(&self, key: &str, addrs: Vec<SocketAddr>) {
            self.answers.lock().unwrap().insert(key.to_owned(), addrs);
        }

        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl HostResolver for FakeHostResolver {
        async fn lookup(&self, addr_str: &str) -> std::io::Result<Vec<SocketAddr>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.answers
                .lock()
                .unwrap()
                .get(addr_str)
                .cloned()
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "no fake answer configured")
                })
        }
    }

    #[tokio::test]
    #[serial(dns_cache_sweep)]
    async fn resolve_socket_addr_with_cache_hit_skips_resolver_call() {
        let resolver = FakeHostResolver::new();
        let key = "fake-cache-test.invalid:5000";
        let first_addr = v4(5000);
        resolver.set(key, vec![first_addr]);

        let resolved = resolve_socket_addr_with(&resolver, key, None)
            .await
            .unwrap();
        assert_eq!(resolved, first_addr);
        assert_eq!(resolver.call_count(), 1, "first call must hit the resolver");

        // Change what the resolver would answer *now* — if the cache is
        // really being consulted (not just producing a coincidentally
        // matching value), the stale `first_addr` must still come back.
        let second_addr = v4(5001);
        resolver.set(key, vec![second_addr]);

        let resolved_again = resolve_socket_addr_with(&resolver, key, None)
            .await
            .unwrap();
        assert_eq!(
            resolved_again, first_addr,
            "a cache hit must return the stale cached address, not re-resolve"
        );
        assert_eq!(
            resolver.call_count(),
            1,
            "a cache hit must not call the resolver again"
        );

        // Force the cached entry older than the TTL (same backdating
        // technique as `dns_cache_lookup_expired_entry_returns_none`), then
        // resolve once more — this must now pick up the resolver's new
        // answer, proving expiry actually triggers re-resolution.
        dns_cache().insert(
            key.to_owned(),
            DnsCacheEntry {
                addrs: vec![first_addr],
                resolved_at: Instant::now() - Duration::from_secs(DNS_CACHE_TTL_SECS + 1),
                next: std::sync::atomic::AtomicUsize::new(0),
            },
        );
        let resolved_after_expiry = resolve_socket_addr_with(&resolver, key, None)
            .await
            .unwrap();
        assert_eq!(
            resolved_after_expiry, second_addr,
            "after the cache entry expires, resolution must reflect the resolver's new answer"
        );
        assert_eq!(
            resolver.call_count(),
            2,
            "an expired entry must trigger exactly one more resolver call"
        );
    }

    #[tokio::test]
    async fn resolve_socket_addr_with_resolver_error_is_not_cached() {
        // A failed resolution must not poison the cache with a bogus entry
        // that a later, successful resolution would then be shadowed by.
        let resolver = FakeHostResolver::new();
        let key = "fake-error-test.invalid:5002";
        // No answer configured for `key` -> FakeHostResolver::lookup errors.
        let result = resolve_socket_addr_with(&resolver, key, None).await;
        assert!(result.is_err());
        assert_eq!(
            dns_cache_lookup(key),
            None,
            "a failed resolution must not populate the cache"
        );
    }

    // ── filter_preferred_family / pick_preferred_addr (Gitar finding on PR #227,
    //    extended for #296's round-robin caching) ────────────────────────────

    fn v4(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn v6(port: u16) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port))
    }

    #[test]
    fn filter_preferred_family_empty_returns_empty() {
        assert_eq!(filter_preferred_family(std::iter::empty()), Vec::new());
    }

    #[test]
    fn filter_preferred_family_only_ipv6_returns_it() {
        let addr = v6(4000);
        assert_eq!(filter_preferred_family(std::iter::once(addr)), vec![addr]);
    }

    #[test]
    fn filter_preferred_family_only_ipv4_returns_it() {
        let addr = v4(4000);
        assert_eq!(filter_preferred_family(std::iter::once(addr)), vec![addr]);
    }

    #[test]
    fn filter_preferred_family_prefers_ipv4_when_ipv6_listed_first() {
        // The exact failure mode Gitar flagged on PR #227: glibc's
        // getaddrinfo (RFC 3484) commonly lists the IPv6 record before the
        // IPv4 one for "localhost", even when the target only listens on
        // IPv4. Taking .next() unconditionally would pick the IPv6 address
        // and fail to connect.
        let ipv4 = v4(4000);
        let ipv6 = v6(4000);
        let filtered = filter_preferred_family(vec![ipv6, ipv4].into_iter());
        assert_eq!(
            filtered,
            vec![ipv4],
            "must prefer IPv4 regardless of order, and drop the IPv6 entry entirely"
        );
    }

    #[test]
    fn filter_preferred_family_prefers_ipv4_when_ipv4_listed_first() {
        let ipv4 = v4(4000);
        let ipv6 = v6(4000);
        let filtered = filter_preferred_family(vec![ipv4, ipv6].into_iter());
        assert_eq!(filtered, vec![ipv4]);
    }

    #[test]
    fn filter_preferred_family_keeps_every_ipv4_address() {
        // Foundation of the round-robin fix (Gitar finding on #296): a
        // multi-A-record hostname (Kubernetes headless service, DNS-based
        // round-robin) must keep *all* its IPv4 candidates, not collapse to
        // the first one — collapsing here is what would have silently
        // frozen every request onto one backend for the cache's full TTL.
        let a = v4(4001);
        let b = v4(4002);
        let c = v4(4003);
        let filtered = filter_preferred_family(vec![a, b, c].into_iter());
        assert_eq!(filtered, vec![a, b, c]);
    }

    #[test]
    fn filter_preferred_family_then_first_of_multiple_prefers_ipv4() {
        // The old single-result `pick_preferred_addr` helper's exact
        // behavior, now expressed as `filter_preferred_family(...).next()`
        // since production code needs the full list (round-robin caching)
        // and no caller needs a single-result wrapper anymore.
        let ipv4 = v4(4000);
        let ipv6 = v6(4000);
        assert_eq!(
            filter_preferred_family(vec![ipv6, ipv4].into_iter())
                .into_iter()
                .next(),
            Some(ipv4)
        );
    }
}
