//! Which addresses a server-side request may reach.
//!
//! One list for every service that fetches a URL someone else gave it: the
//! link proxy (january) and the bot interaction endpoints (delta).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Private, loopback, link-local, shared (carrier NAT), test, multicast and
/// reserved IPv4 ranges
const BLOCKED_V4: &[(Ipv4Addr, u8)] = &[
    (Ipv4Addr::new(0, 0, 0, 0), 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(192, 0, 2, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    (Ipv4Addr::new(198, 51, 100, 0), 24),
    (Ipv4Addr::new(203, 0, 113, 0), 24),
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];

/// The same for IPv6, with the forms that can carry an IPv4 address inside
const BLOCKED_V6: &[(u128, u8)] = &[
    // unspecified, loopback and IPv4-compatible (::a.b.c.d)
    (0, 96),
    // IPv4-mapped (::ffff:a.b.c.d)
    (0xffff_0000_0000, 96),
    (0x0064_ff9b << 96, 96),
    (0x0064_ff9b_0001 << 80, 48),
    (0x0100 << 112, 64),
    (0x2001_0db8 << 96, 32),
    (0x2002 << 112, 16),
    (0xfc00 << 112, 7),
    (0xfe80 << 112, 10),
    // deprecated site-local
    (0xfec0 << 112, 10),
    (0xff00 << 112, 8),
];

fn v4_is_public(ip: Ipv4Addr) -> bool {
    let ip = u32::from(ip);
    !BLOCKED_V4.iter().any(|(net, bits)| {
        let mask = u32::MAX.checked_shl(32 - *bits as u32).unwrap_or(0);
        ip & mask == u32::from(*net) & mask
    })
}

fn v6_is_public(ip: Ipv6Addr) -> bool {
    let bits = u128::from(ip);
    !BLOCKED_V6.iter().any(|(net, prefix)| {
        let mask = u128::MAX.checked_shl(128 - *prefix as u32).unwrap_or(0);
        bits & mask == *net & mask
    })
}

/// Whether a server-side request may connect to this address
pub fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => v4_is_public(*ip),
        // an IPv4 address wrapped in IPv6 must pass the IPv4 ranges too
        IpAddr::V6(ip) => {
            v6_is_public(*ip) && ip.to_ipv4_mapped().is_none_or(v4_is_public)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_public_ip;

    #[test]
    fn inside_addresses_are_blocked() {
        for ip in [
            "0.0.0.0", "10.0.0.1", "10.255.255.255", "100.64.0.1", "100.127.255.255", "127.0.0.1",
            "169.254.169.254", "172.16.0.1", "172.31.255.255", "192.0.0.1", "192.0.2.1",
            "192.168.1.1", "198.18.0.1", "198.19.255.255", "198.51.100.1", "203.0.113.1",
            "224.0.0.1", "239.255.255.255", "240.0.0.1", "255.255.255.255", "::", "::1",
            "::127.0.0.1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:169.254.169.254",
            "64:ff9b::7f00:1", "64:ff9b:1::1", "100::1", "2001:db8::1", "2002:7f00:1::",
            "fc00::1", "fd00::1", "fe80::1", "febf::1", "fec0::1", "ff02::1",
        ] {
            assert!(!is_public_ip(&ip.parse().unwrap()), "{ip} must be blocked");
        }
    }

    #[test]
    fn public_addresses_pass() {
        for ip in [
            "1.1.1.1", "8.8.8.8", "93.184.216.34", "100.128.0.1", "172.32.0.1", "198.20.0.1",
            "2606:4700:4700::1111", "2a00:1450:4001::1",
        ] {
            assert!(is_public_ip(&ip.parse().unwrap()), "{ip} must pass");
        }
    }
}
