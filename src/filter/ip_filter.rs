use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use pingora_proxy::Session;

use crate::config::schema::IpFilterConfig;

/// Returns `true` if the request is allowed through, `false` if it should be blocked (403).
pub fn is_allowed(config: &IpFilterConfig, session: &Session) -> bool {
    let trust_proxy = config.trust_proxy.unwrap_or(false);
    let client_ip = client_ip(session, trust_proxy);

    match (&config.allow, &config.deny) {
        // Whitelist mode: IP must be in allow list
        (Some(allow), _) => client_ip
            .map(|ip| allow.iter().any(|r| matches_rule(&ip, r)))
            .unwrap_or(false),

        // Blacklist mode: block if in deny list
        (None, Some(deny)) => client_ip
            .map(|ip| !deny.iter().any(|r| matches_rule(&ip, r)))
            .unwrap_or(true),

        // No rules configured — allow everything
        (None, None) => true,
    }
}

fn client_ip(session: &Session, trust_proxy: bool) -> Option<IpAddr> {
    if trust_proxy {
        if let Some(xff) = session.req_header().headers.get("x-forwarded-for") {
            if let Ok(s) = xff.to_str() {
                if let Some(first) = s.split(',').next() {
                    if let Ok(ip) = first.trim().parse::<IpAddr>() {
                        return Some(ip);
                    }
                }
            }
        }
    }
    session.client_addr().and_then(|a| a.as_inet()).map(|a| a.ip())
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
}
