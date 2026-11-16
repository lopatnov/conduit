//! CIDR and exact-address matching, shared by every feature that takes a list of IP addresses and networks
//! (`ipFilter`, the PROXY protocol's `trustedSources`).
//!
//! A rule is an address (`"203.0.113.5"`, `"2001:db8::1"`) or a network (`"10.0.0.0/8"`, `"2001:db8::/32"`). An IPv4-mapped
//! IPv6 address (`::ffff:a.b.c.d`) is the IPv4 address it carries, on both sides of the comparison.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// `true` when `rule` is a valid address or a valid CIDR block (prefix at most 32 for IPv4, 128 for IPv6).
pub fn is_valid_ip_or_cidr(s: &str) -> bool {
    if let Some((addr, prefix)) = s.split_once('/') {
        let Ok(ip) = addr.parse::<IpAddr>() else {
            return false;
        };
        let Ok(prefix_len) = prefix.parse::<u32>() else {
            return false;
        };
        let max_prefix = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        prefix_len <= max_prefix
    } else {
        s.parse::<IpAddr>().is_ok()
    }
}

/// `true` when `ip` is the address `rule` names or lies inside the network it names. An invalid rule matches nothing.
pub fn matches_rule(ip: &IpAddr, rule: &str) -> bool {
    if let Some((addr_s, prefix_s)) = rule.split_once('/') {
        if let Ok(prefix) = prefix_s.parse::<u32>() {
            if let Ok(net) = addr_s.parse::<IpAddr>() {
                return in_subnet(ip, &net, prefix);
            }
        }
        return false;
    }
    // Normalize both sides so an IPv4-mapped IPv6 address (e.g. from a
    // trustProxy XFF chain) matches a plain IPv4 rule — mirrors the
    // normalization `in_subnet` already does for the CIDR branch above.
    rule.parse::<IpAddr>()
        .map(|a| a.to_canonical() == ip.to_canonical())
        .unwrap_or(false)
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
    fn ipv4_mapped_ipv6_matches_plain_ipv4_rule() {
        // ::ffff:203.0.113.5 is the IPv4-mapped IPv6 form of 203.0.113.5.
        // A plain (non-CIDR) rule of "203.0.113.5" must still match it —
        // the equivalent CIDR rule "203.0.113.5/32" already does via
        // in_subnet's normalization; the exact-match branch must too.
        let ip: IpAddr = "::ffff:203.0.113.5".parse().unwrap();
        assert!(matches_rule(&ip, "203.0.113.5"));
        assert!(!matches_rule(&ip, "203.0.113.6"));
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
}
