//! Network facts: which peers count as "home network", which addresses this computer has.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket};

/// True for addresses that cannot be reached from the public internet without a VPN or a
/// port forward: loopback, private ranges (RFC 1918), link-local, carrier-grade NAT (RFC 6598,
/// used by some VPNs such as Tailscale) and IPv6 unique-local/link-local addresses.
pub fn is_local_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_local_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local_v4(v4);
            }
            let s = v6.segments();
            v6.is_loopback()
                || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
        }
    }
}

fn is_local_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10
}

/// Key for per-address limits: IPv4 as it is, IPv6 by its /64 network (one host can use any number of addresses
/// of its /64, they must not each get their own allowance).
pub fn limit_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        IpAddr::V6(v6) => IpAddr::V4(v6.to_ipv4_mapped().unwrap()),
        v4 => v4,
    }
}

/// Addresses of this computer that a device in the home network can use (no loopback),
/// IPv4 first. Never empty when the computer has a network connection.
pub fn local_addresses() -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    #[cfg(unix)]
    unix_addresses(&mut out);
    // address of the default route (works on every platform; sends nothing)
    if let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        if sock.connect((Ipv4Addr::new(10, 255, 255, 255), 9)).is_ok() {
            if let Ok(a) = sock.local_addr() {
                if !a.ip().is_unspecified() && !a.ip().is_loopback() && !out.contains(&a.ip()) {
                    out.insert(0, a.ip());
                }
            }
        }
    }
    out.retain(|ip| !ip.is_loopback() && is_local_network(*ip));
    out.sort_by_key(|ip| match ip {
        IpAddr::V4(v4) if v4.is_private() => 0,
        IpAddr::V4(_) => 1,
        IpAddr::V6(_) => 2,
    });
    out.dedup();
    out
}

#[cfg(unix)]
fn unix_addresses(out: &mut Vec<IpAddr>) {
    // SAFETY: getifaddrs fills a linked list that freeifaddrs releases; every node is only read.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return;
        }
        let mut node = list;
        while !node.is_null() {
            let ifa = &*node;
            let up = (ifa.ifa_flags & libc::IFF_UP as libc::c_uint) != 0;
            if up && !ifa.ifa_addr.is_null() {
                let family = (*ifa.ifa_addr).sa_family as i32;
                if family == libc::AF_INET {
                    let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    out.push(IpAddr::V4(Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr))));
                } else if family == libc::AF_INET6 {
                    let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                    let ip = Ipv6Addr::from(sa.sin6_addr.s6_addr);
                    // link-local IPv6 needs a zone id in URLs; not useful for a browser
                    if (ip.segments()[0] & 0xffc0) != 0xfe80 {
                        out.push(IpAddr::V6(ip));
                    }
                }
            }
            node = ifa.ifa_next;
        }
        libc::freeifaddrs(list);
    }
}

/// Bonjour name of this computer (`name.local`), if the system has one.
pub fn local_host_name() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is large enough and NUL-terminated by gethostname on success.
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0;
        if !ok {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let name = String::from_utf8_lossy(&buf[..end]).trim().to_ascii_lowercase();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
            return None;
        }
        let name = if name.ends_with(".local") { name } else { format!("{}.local", name.split('.').next()?) };
        Some(name)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// URL text of an address with port (`https://192.168.1.5:47820`, IPv6 in brackets).
pub fn url_for(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(v4) => format!("https://{v4}:{port}"),
        IpAddr::V6(v6) => format!("https://[{v6}]:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_limits_per_64() {
        assert_eq!(limit_key("fd00:1:2:3:aaaa::1".parse().unwrap()), limit_key("fd00:1:2:3:bbbb::9".parse().unwrap()));
        assert_ne!(limit_key("fd00:1:2:3::1".parse().unwrap()), limit_key("fd00:1:2:4::1".parse().unwrap()));
        assert_eq!(limit_key("192.168.1.2".parse().unwrap()), "192.168.1.2".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn home_network_ranges() {
        for ok in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.178.20", "169.254.1.1", "100.64.0.1", "100.127.255.255", "::1", "fd00::1", "fe80::1", "::ffff:192.168.1.2"] {
            assert!(is_local_network(ok.parse().unwrap()), "{ok}");
        }
        for bad in ["8.8.8.8", "172.32.0.1", "100.128.0.1", "1.1.1.1", "2001:db8::1", "2a00:1450::1", "::ffff:8.8.8.8", "0.0.0.0"] {
            assert!(!is_local_network(bad.parse().unwrap()), "{bad}");
        }
    }
}
