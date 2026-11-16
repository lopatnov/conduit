//! Connection limits of a `tcp` site (issue #757): a cap on the connections of the site and a cap per client address.
//!
//! The check runs when a connection is accepted, before any upstream connect, and a refused connection is closed at once
//! (nginx `limit_conn`, Envoy `connection_limit`; not queued, as HAProxy `maxconn` does, because a queue hides overload and
//! holds sockets). A permit is released when it is dropped, whichever way the relay ends.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex, PoisonError};

use crate::throttle::Throttle;

/// Why a connection was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `maxConnections` connections are open on the site.
    SiteFull,
    /// `maxConnectionsPerIp` connections are open from this client.
    ClientFull,
}

/// The key the per-client count is kept under: an IPv4 address as it is, an IPv4-mapped IPv6 address as the IPv4
/// address it carries, any other IPv6 address as its /64 (one /64 is one subscriber's whole network, so counting single
/// /128 addresses would let one client open as many connections as it has addresses).
pub fn client_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let mut octets = v6.octets();
            octets[8..].fill(0);
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        v4 => v4,
    }
}

/// The open connections of a site: the total, and (only when a per-client cap is set) the count of each client.
#[derive(Default)]
struct State {
    open: usize,
    per_ip: HashMap<IpAddr, usize>,
}

/// The caps and the open counts of one site.
pub struct Limits {
    max_connections: Option<usize>,
    max_per_ip: Option<usize>,
    // One lock for both counts, so that a refusal never leaves a slot reserved for a moment (a refused connection
    // must not make a legitimate one look over the cap). Connections are accepted at a human rate, not per request.
    state: Mutex<State>,
    /// Decides which refusals are logged: one line per interval, so a flood does not become a flood of log lines.
    throttle: Throttle,
}

impl Limits {
    pub fn new(max_connections: Option<u32>, max_per_ip: Option<u32>) -> Arc<Self> {
        Arc::new(Self {
            max_connections: max_connections.map(|n| n as usize),
            max_per_ip: max_per_ip.map(|n| n as usize),
            state: Mutex::new(State::default()),
            throttle: Throttle::default(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admit a connection from `client` (`None` when its address is not known: only the site cap applies), or say why not.
    pub fn admit(self: &Arc<Self>, client: Option<IpAddr>) -> Result<Permit, Refusal> {
        let mut state = self.lock();
        if self.max_connections.is_some_and(|max| state.open >= max) {
            return Err(Refusal::SiteFull);
        }
        let key = client.map(client_key).filter(|_| self.max_per_ip.is_some());
        if let (Some(key), Some(max)) = (key, self.max_per_ip) {
            let count = state.per_ip.entry(key).or_insert(0);
            if *count >= max {
                // `or_insert(0)` made an empty entry for a client that has no connection: take it back out
                if *count == 0 {
                    state.per_ip.remove(&key);
                }
                return Err(Refusal::ClientFull);
            }
            *count += 1;
        }
        state.open += 1;
        Ok(Permit {
            limits: Arc::clone(self),
            key,
        })
    }

    /// Connections open on the site now.
    pub fn open(&self) -> usize {
        self.lock().open
    }

    /// Count a refusal; `Some(n)` when it is time to log (`n` refusals since the last logged one).
    pub fn note_refusal(&self) -> Option<u64> {
        self.throttle.note()
    }
}

/// An admitted connection. Dropping it frees the slot.
pub struct Permit {
    limits: Arc<Limits>,
    key: Option<IpAddr>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self.limits.lock();
        if let Some(key) = self.key {
            if let Some(count) = state.per_ip.get_mut(&key) {
                *count -= 1;
                if *count == 0 {
                    state.per_ip.remove(&key);
                }
            }
        }
        state.open -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn no_caps_admit_everything() {
        let l = Limits::new(None, None);
        let permits: Vec<_> = (0..1000)
            .map(|_| l.admit(Some(ip("192.0.2.1"))).unwrap())
            .collect();
        assert_eq!(l.open(), 1000);
        drop(permits);
        assert_eq!(l.open(), 0);
        assert!(
            l.lock().per_ip.is_empty(),
            "no per-client state without a per-client cap"
        );
    }

    #[test]
    fn the_site_cap_refuses_the_connection_over_it_and_frees_on_drop() {
        let l = Limits::new(Some(2), None);
        let a = l.admit(Some(ip("192.0.2.1"))).unwrap();
        let _b = l.admit(Some(ip("192.0.2.2"))).unwrap();
        assert_eq!(
            l.admit(Some(ip("192.0.2.3"))).err(),
            Some(Refusal::SiteFull)
        );
        assert_eq!(l.open(), 2, "a refused connection holds no slot");
        drop(a);
        assert!(
            l.admit(Some(ip("192.0.2.3"))).is_ok(),
            "a closed connection frees its slot"
        );
    }

    #[test]
    fn the_per_client_cap_is_per_address_and_frees_on_drop() {
        let l = Limits::new(None, Some(2));
        let a = l.admit(Some(ip("192.0.2.1"))).unwrap();
        let _b = l.admit(Some(ip("192.0.2.1"))).unwrap();
        assert_eq!(
            l.admit(Some(ip("192.0.2.1"))).err(),
            Some(Refusal::ClientFull)
        );
        let _c = l
            .admit(Some(ip("192.0.2.2")))
            .expect("another client is not affected");
        assert_eq!(l.open(), 3, "a refused connection holds no slot");
        drop(a);
        assert!(l.admit(Some(ip("192.0.2.1"))).is_ok());
    }

    #[test]
    fn the_per_client_map_does_not_grow_with_closed_connections() {
        let l = Limits::new(None, Some(5));
        for n in 0..200u32 {
            let _p = l
                .admit(Some(IpAddr::from([10, 0, (n >> 8) as u8, n as u8])))
                .unwrap();
        }
        assert!(l.lock().per_ip.is_empty());
    }

    #[test]
    fn ipv6_clients_are_counted_per_64_and_mapped_addresses_as_ipv4() {
        assert_eq!(
            client_key(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd")),
            ip("2001:db8:1:2::")
        );
        assert_eq!(
            client_key(ip("2001:db8:1:2::1")),
            client_key(ip("2001:db8:1:2:ffff::9"))
        );
        assert_ne!(
            client_key(ip("2001:db8:1:2::1")),
            client_key(ip("2001:db8:1:3::1"))
        );
        assert_eq!(client_key(ip("::ffff:192.0.2.7")), ip("192.0.2.7"));
        let l = Limits::new(None, Some(1));
        let _p = l.admit(Some(ip("2001:db8:1:2::1"))).unwrap();
        assert_eq!(
            l.admit(Some(ip("2001:db8:1:2::2"))).err(),
            Some(Refusal::ClientFull),
            "a second address of the same /64 is the same client"
        );
        let _q = l.admit(Some(ip("192.0.2.7"))).unwrap();
        assert_eq!(
            l.admit(Some(ip("::ffff:192.0.2.7"))).err(),
            Some(Refusal::ClientFull),
            "the IPv4-mapped form of an address is the same client"
        );
    }

    #[test]
    fn an_unknown_client_is_held_to_the_site_cap_only() {
        let l = Limits::new(Some(1), Some(1));
        let _a = l.admit(None).unwrap();
        assert_eq!(l.admit(None).err(), Some(Refusal::SiteFull));
    }

    #[test]
    fn refusals_are_logged_at_a_limited_rate() {
        let mut l = Limits::new(Some(0), None);
        Arc::get_mut(&mut l).unwrap().throttle = Throttle::new(Duration::from_millis(300));
        assert_eq!(l.note_refusal(), Some(1), "the first one is logged");
        for _ in 0..50 {
            assert_eq!(l.note_refusal(), None, "then none for a while");
        }
        std::thread::sleep(Duration::from_millis(350));
        let n = l
            .note_refusal()
            .expect("after the interval the next one is logged");
        assert_eq!(n, 51, "with the number of refusals since the last line");
    }

    #[test]
    fn concurrent_admissions_never_exceed_the_cap() {
        let l = Limits::new(Some(8), Some(4));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles: Vec<_> = (0..16)
            .map(|t| {
                let (l, peak) = (Arc::clone(&l), Arc::clone(&peak));
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        if let Ok(p) = l.admit(Some(IpAddr::from([192, 0, 2, (t % 4) as u8]))) {
                            peak.fetch_max(l.open(), Ordering::Relaxed);
                            drop(p);
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(
            peak.load(Ordering::Relaxed) <= 8,
            "peak {}",
            peak.load(Ordering::Relaxed)
        );
        assert_eq!(l.open(), 0);
        assert!(l.lock().per_ip.is_empty());
    }
}
