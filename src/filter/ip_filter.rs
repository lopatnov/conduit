use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use pingora_proxy::Session;

use crate::config::schema::IpFilterConfig;

/// Returns `true` if the request is allowed through, `false` if it should be blocked (403).
pub fn is_allowed(config: &IpFilterConfig, session: &Session) -> bool {
    let trust_proxy = config.trust_proxy.unwrap_or(false);
    let ip = client_ip(session, trust_proxy);
    apply_ip_filter(ip, config)
}

/// Pure filtering logic: given a resolved client IP (or `None` when unknown),
/// decide whether the request passes the allow/deny rules.
///
/// Extracted for unit testability — `is_allowed` is a thin wrapper that
/// resolves the IP from the Pingora session and then calls this.
pub(crate) fn apply_ip_filter(ip: Option<IpAddr>, config: &IpFilterConfig) -> bool {
    match (&config.allow, &config.deny) {
        // Whitelist mode: IP must be in allow list
        (Some(allow), _) => ip
            .map(|ip| allow.iter().any(|r| matches_rule(&ip, r)))
            .unwrap_or(false),

        // Blacklist mode: block if in deny list
        (None, Some(deny)) => ip
            .map(|ip| !deny.iter().any(|r| matches_rule(&ip, r)))
            .unwrap_or(true),

        // No rules configured — allow everything
        (None, None) => true,
    }
}

/// Parse the trusted client IP from an `X-Forwarded-For` header value.
///
/// Takes the **rightmost** entry — the IP appended by the immediately adjacent
/// trusted proxy.  This is the only entry a trusted proxy can vouch for.
///
/// The leftmost entry is **attacker-controlled**: a client behind a real proxy
/// can forge `X-Forwarded-For: 1.1.1.1` and the proxy will prepend it, making
/// the leftmost entry `1.1.1.1` (spoofed).  The rightmost entry is added by
/// the trusted proxy itself and cannot be forged by the downstream client.
///
/// ```text
/// Attacker sends:   X-Forwarded-For: 1.1.1.1
/// Trusted proxy adds its view of the client IP (e.g. 5.5.5.5):
///   X-Forwarded-For: 1.1.1.1, 5.5.5.5
/// Leftmost = 1.1.1.1  ← FORGED
/// Rightmost = 5.5.5.5 ← added by trusted proxy, safe to use
/// ```
///
/// Extracted for unit testability — `client_ip` calls this when `trust_proxy` is enabled.
pub(crate) fn parse_xff(xff: &str) -> Option<IpAddr> {
    // Use `next_back()` directly — `split(',')` is a `DoubleEndedIterator` so
    // this avoids needlessly traversing all elements to find the last.
    xff.split(',').next_back()?.trim().parse().ok()
}

/// Public wrapper for testability — returns the effective client IP.
pub(crate) fn client_ip_for_check(session: &Session, trust_proxy: bool) -> Option<IpAddr> {
    client_ip(session, trust_proxy)
}

/// Check whether `ip` is blocked by any rule in `rules` (deny-list mode).
///
/// Used by `IpGuard::is_dynamic_denied` to avoid cloning the deny list Vec.
pub(crate) fn is_in_deny_list(ip: Option<IpAddr>, rules: &[String]) -> bool {
    match ip {
        None => false, // unknown IP passes
        Some(ip) => rules.iter().any(|r| matches_rule(&ip, r)),
    }
}

fn client_ip(session: &Session, trust_proxy: bool) -> Option<IpAddr> {
    if trust_proxy {
        if let Some(xff) = session.req_header().headers.get("x-forwarded-for") {
            if let Ok(s) = xff.to_str() {
                if let Some(ip) = parse_xff(s) {
                    return Some(ip);
                }
            }
        }
    }
    session
        .client_addr()
        .and_then(|a| a.as_inet())
        .map(|a| a.ip())
}

fn matches_rule(ip: &IpAddr, rule: &str) -> bool {
    if let Some((addr_s, prefix_s)) = rule.split_once('/') {
        if let Ok(prefix) = prefix_s.parse::<u32>() {
            if let Ok(net) = addr_s.parse::<IpAddr>() {
                return in_subnet(ip, &net, prefix);
            }
        }
        return false;
    }
    rule.parse::<IpAddr>().map(|a| a == *ip).unwrap_or(false)
}

fn in_subnet(ip: &IpAddr, net: &IpAddr, prefix: u32) -> bool {
    match (ip, net) {
        (IpAddr::V4(ip4), IpAddr::V4(net4)) => ipv4_in_subnet(*ip4, *net4, prefix),
        (IpAddr::V6(ip6), IpAddr::V6(net6)) => ipv6_in_subnet(*ip6, *net6, prefix),
        // Handle IPv4-mapped IPv6 addresses (e.g. ::ffff:127.0.0.1 vs 127.0.0.1)
        (IpAddr::V6(ip6), IpAddr::V4(net4)) => {
            if let Some(ip4) = ip6.to_ipv4_mapped() {
                ipv4_in_subnet(ip4, *net4, prefix)
            } else {
                false
            }
        }
        (IpAddr::V4(ip4), IpAddr::V6(net6)) => {
            if let Some(net4) = net6.to_ipv4_mapped() {
                ipv4_in_subnet(*ip4, net4, prefix)
            } else {
                false
            }
        }
    }
}

fn ipv4_in_subnet(ip: Ipv4Addr, net: Ipv4Addr, prefix: u32) -> bool {
    if prefix == 0 {
        return true;
    }
    if prefix > 32 {
        return false;
    }
    let mask = u32::MAX << (32 - prefix);
    (u32::from(ip) & mask) == (u32::from(net) & mask)
}

fn ipv6_in_subnet(ip: Ipv6Addr, net: Ipv6Addr, prefix: u32) -> bool {
    if prefix == 0 {
        return true;
    }
    if prefix > 128 {
        return false;
    }
    let mask = u128::MAX << (128 - prefix);
    (u128::from(ip) & mask) == (u128::from(net) & mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_ipv4_match() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(matches_rule(&ip, "127.0.0.1"));
        assert!(!matches_rule(&ip, "127.0.0.2"));
    }

    #[test]
    fn cidr_ipv4() {
        let ip: IpAddr = "192.168.1.100".parse().unwrap();
        assert!(matches_rule(&ip, "192.168.1.0/24"));
        assert!(matches_rule(&ip, "192.168.0.0/16"));
        assert!(matches_rule(&ip, "192.0.0.0/8"));
        assert!(matches_rule(&ip, "0.0.0.0/0"));
        assert!(!matches_rule(&ip, "10.0.0.0/8"));
    }

    #[test]
    fn cidr_ipv4_prefix32_exact() {
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(matches_rule(&ip, "10.0.0.1/32"));
        assert!(!matches_rule(&ip, "10.0.0.2/32"));
    }

    #[test]
    fn cidr_ipv4_prefix0_matches_all() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(matches_rule(&ip, "0.0.0.0/0"));
    }

    #[test]
    fn exact_ipv6_match() {
        let ip: IpAddr = "::1".parse().unwrap();
        assert!(matches_rule(&ip, "::1"));
        assert!(!matches_rule(&ip, "::2"));
    }

    #[test]
    fn cidr_ipv6() {
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        assert!(matches_rule(&ip, "2001:db8::/32"));
        assert!(!matches_rule(&ip, "2001:db9::/32"));
    }

    #[test]
    fn ipv4_mapped_ipv6_matches_ipv4_subnet() {
        // ::ffff:127.0.0.1 is the IPv4-mapped IPv6 form of 127.0.0.1.
        // It should match an IPv4 CIDR rule.
        let ip: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(matches_rule(&ip, "127.0.0.0/8"));
        assert!(!matches_rule(&ip, "10.0.0.0/8"));
    }

    #[test]
    fn ipv6_not_mapped_does_not_match_ipv4_subnet() {
        // A regular IPv6 address (not IPv4-mapped) must NOT match an IPv4 rule.
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        assert!(!matches_rule(&ip, "127.0.0.0/8"));
    }

    #[test]
    fn ipv4_does_not_match_ipv6_subnet() {
        // An IPv4 address must not match a non-IPv4-mapped IPv6 network.
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(!matches_rule(&ip, "2001:db8::/32"));
    }

    #[test]
    fn invalid_cidr_address_returns_false() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        // "not-an-ip/24" — address part fails to parse → false
        assert!(!matches_rule(&ip, "not-an-ip/24"));
        // "1.2.3.4/abc" — prefix part fails to parse → false
        assert!(!matches_rule(&ip, "1.2.3.4/abc"));
    }

    #[test]
    fn ipv4_prefix_zero_matches_all() {
        let ip: IpAddr = "203.0.113.5".parse().unwrap();
        assert!(ipv4_in_subnet(
            "203.0.113.5".parse().unwrap(),
            "0.0.0.0".parse().unwrap(),
            0
        ));
        let _ = ip; // silence unused warning
    }

    #[test]
    fn ipv6_prefix_zero_matches_all() {
        assert!(ipv6_in_subnet(
            "2001:db8::1".parse().unwrap(),
            "::".parse().unwrap(),
            0
        ));
    }

    #[test]
    fn ipv4_prefix_above_32_returns_false() {
        assert!(!ipv4_in_subnet(
            "1.2.3.4".parse().unwrap(),
            "1.2.3.4".parse().unwrap(),
            33
        ));
    }

    #[test]
    fn ipv6_prefix_above_128_returns_false() {
        assert!(!ipv6_in_subnet(
            "::1".parse().unwrap(),
            "::1".parse().unwrap(),
            129
        ));
    }

    // ── apply_ip_filter ───────────────────────────────────────────────────────

    fn cfg_allow(cidrs: &[&str]) -> IpFilterConfig {
        IpFilterConfig {
            allow: Some(cidrs.iter().map(|s| s.to_string()).collect()),
            deny: None,
            trust_proxy: None,
            dry_run: None,
        }
    }

    fn cfg_deny(cidrs: &[&str]) -> IpFilterConfig {
        IpFilterConfig {
            allow: None,
            deny: Some(cidrs.iter().map(|s| s.to_string()).collect()),
            trust_proxy: None,
            dry_run: None,
        }
    }

    fn cfg_none() -> IpFilterConfig {
        IpFilterConfig {
            allow: None,
            deny: None,
            trust_proxy: None,
            dry_run: None,
        }
    }

    #[test]
    fn whitelist_ip_in_list_is_allowed() {
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(apply_ip_filter(Some(ip), &cfg_allow(&["10.0.0.0/8"])));
    }

    #[test]
    fn whitelist_ip_not_in_list_is_denied() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(!apply_ip_filter(Some(ip), &cfg_allow(&["10.0.0.0/8"])));
    }

    #[test]
    fn whitelist_unknown_ip_is_denied() {
        // When IP cannot be resolved, whitelist mode must deny (fail-closed).
        assert!(!apply_ip_filter(None, &cfg_allow(&["10.0.0.0/8"])));
    }

    #[test]
    fn blacklist_ip_in_list_is_denied() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(!apply_ip_filter(Some(ip), &cfg_deny(&["1.2.3.4"])));
    }

    #[test]
    fn blacklist_ip_not_in_list_is_allowed() {
        let ip: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(apply_ip_filter(Some(ip), &cfg_deny(&["1.2.3.4"])));
    }

    #[test]
    fn blacklist_unknown_ip_is_allowed() {
        // When IP cannot be resolved, blacklist mode must allow (fail-open).
        assert!(apply_ip_filter(None, &cfg_deny(&["1.2.3.4"])));
    }

    #[test]
    fn no_rules_always_allows() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(apply_ip_filter(Some(ip), &cfg_none()));
        assert!(apply_ip_filter(None, &cfg_none()));
    }

    // ── parse_xff ─────────────────────────────────────────────────────────────

    #[test]
    fn xff_single_ip() {
        let ip = parse_xff("203.0.113.1");
        assert_eq!(ip, Some("203.0.113.1".parse().unwrap()));
    }

    #[test]
    fn xff_rightmost_of_multiple() {
        // Security: rightmost is added by the trusted proxy, cannot be forged.
        // A client behind a proxy could send "X-Forwarded-For: 1.1.1.1" and the
        // proxy appends ", real_client_ip" — we must trust the rightmost, not the left.
        let ip = parse_xff("203.0.113.1, 10.0.0.1, 192.168.1.1");
        assert_eq!(
            ip,
            Some("192.168.1.1".parse().unwrap()),
            "must return rightmost (trusted-proxy-appended) IP"
        );
    }

    #[test]
    fn xff_spoof_protection() {
        // Attacker forges: X-Forwarded-For: 8.8.8.8
        // Trusted proxy appends real client IP: 5.5.5.5
        // Result: "8.8.8.8, 5.5.5.5" — rightmost is safe
        let ip = parse_xff("8.8.8.8, 5.5.5.5");
        assert_eq!(
            ip,
            Some("5.5.5.5".parse().unwrap()),
            "forged leftmost must not be used"
        );
    }

    #[test]
    fn xff_invalid_ip_returns_none() {
        assert_eq!(parse_xff("not-an-ip"), None);
    }

    #[test]
    fn xff_empty_string_returns_none() {
        assert_eq!(parse_xff(""), None);
    }

    #[test]
    fn xff_ipv6() {
        let ip = parse_xff("203.0.113.1, 2001:db8::1");
        assert_eq!(ip, Some("2001:db8::1".parse().unwrap()));
    }

    // ── dynamic deny-list (IpFilterConfig denylist) ───────────────────────────

    #[test]
    fn dynamic_deny_blocks_matching_cidr() {
        let cfg = IpFilterConfig {
            allow: None,
            deny: Some(vec!["192.168.1.0/24".to_owned()]),
            trust_proxy: None,
            dry_run: None,
        };
        // IP in denied range → blocked
        assert!(!apply_ip_filter(
            Some("192.168.1.42".parse().unwrap()),
            &cfg
        ));
        // IP outside denied range → allowed
        assert!(apply_ip_filter(
            Some("10.0.0.1".parse().unwrap()),
            &cfg
        ));
    }

    #[test]
    fn dynamic_deny_blocks_exact_ip() {
        let cfg = IpFilterConfig {
            allow: None,
            deny: Some(vec!["203.0.113.5".to_owned()]),
            trust_proxy: None,
            dry_run: None,
        };
        assert!(!apply_ip_filter(
            Some("203.0.113.5".parse().unwrap()),
            &cfg
        ));
        assert!(apply_ip_filter(
            Some("203.0.113.6".parse().unwrap()),
            &cfg
        ));
    }

    #[test]
    fn empty_dynamic_deny_allows_all() {
        let cfg = IpFilterConfig::default();
        assert!(apply_ip_filter(
            Some("1.2.3.4".parse().unwrap()),
            &cfg
        ));
    }
}
