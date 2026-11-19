//! SSRF guard shared by every surface that connects to a user- or model-supplied host.
//!
//! Single responsibility: classify IP addresses and host names ([`HostClass`]), resolve a host
//! and refuse it when any resolved address is in a class the caller does not allow, and match
//! hosts against `NetworkSettings.no_proxy`. Two refusal tiers: [`HostClass::LinkLocalOrMetadata`]
//! (link-local, cloud metadata, multicast, unspecified, reserved and documentation ranges,
//! Teredo, local-use NAT64, and IPv6 forms (IPv4-mapped, NAT64, SIIT, 6to4) embedding such an
//! IPv4 address) is refused on every surface; loopback, private
//! (RFC 1918, ULA) and CGNAT addresses are refused unless the caller opts in (a user-confirmed
//! setting).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use url::Host;

use crate::error::AppError;

/// What kind of network a host or address belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostClass {
    /// `127.0.0.0/8`, `::1`, `localhost`, `*.localhost`.
    Loopback,
    /// RFC 1918, IPv6 unique-local (`fc00::/7`) and deprecated site-local (`fec0::/10`).
    PrivateRfc1918,
    /// Carrier-grade NAT, `100.64.0.0/10` (except the `100.100.100.200` metadata address).
    Cgnat,
    /// Never reachable through a guarded connection: link-local (`169.254.0.0/16`,
    /// `fe80::/10`), cloud metadata (`169.254.169.254`, `fd00:ec2::254`, `100.100.100.200`,
    /// `metadata.google.internal`), multicast, unspecified, `0.0.0.0/8`, broadcast, reserved
    /// (`240.0.0.0/4`, `192.0.0.0/24`, `100::/64`) and documentation (`192.0.2.0/24`,
    /// `198.51.100.0/24`, `203.0.113.0/24`, `2001:db8::/32`) ranges.
    LinkLocalOrMetadata,
    /// Everything else, including `198.18.0.0/15`: the fake-IP block of TUN-mode proxies (Clash,
    /// Surge) answers every name with an address there.
    Public,
}

/// The class of `ip`; IPv6 addresses embedding an IPv4 address (`::ffff:a.b.c.d`, `::a.b.c.d`,
/// `64:ff9b::/96`, `::ffff:0:a.b.c.d` (SIIT), 6to4 `2002::/16`) take the class of the embedded
/// address. Teredo (`2001::/32`) and the local-use NAT64 block (`64:ff9b:1::/48`, whose embedding
/// depends on the operator's prefix length) are refused outright.
#[must_use]
pub fn classify_ip(ip: IpAddr) -> HostClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            let low_v4 = || Ipv4Addr::from(u32::from(s[6]) << 16 | u32::from(s[7]));
            let embedded_v4 = if s[..5] == [0; 5] && (s[5] == 0xffff || s[5] == 0)
                || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0]
                || (s[..4] == [0; 4] && s[4] == 0xffff && s[5] == 0)
            {
                Some(low_v4())
            } else if s[0] == 0x2002 {
                Some(Ipv4Addr::from(u32::from(s[1]) << 16 | u32::from(s[2])))
            } else {
                None
            };
            if v6.is_unspecified()
                || v6.is_multicast()
                || v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254) // AWS IPv6 metadata
                || s[0] & 0xffc0 == 0xfe80 // fe80::/10 link-local
                || s[..2] == [0x2001, 0x0db8] // 2001:db8::/32 documentation
                || s[..2] == [0x2001, 0] // 2001::/32 Teredo
                || s[..3] == [0x64, 0xff9b, 1] // 64:ff9b:1::/48 local-use NAT64
                || s[..4] == [0x0100, 0, 0, 0]
            // 100::/64 discard-only
            {
                HostClass::LinkLocalOrMetadata
            } else if v6.is_loopback() {
                HostClass::Loopback
            } else if let Some(v4) = embedded_v4 {
                classify_v4(v4)
            } else if s[0] & 0xfe00 == 0xfc00 || s[0] & 0xffc0 == 0xfec0 {
                HostClass::PrivateRfc1918
            } else {
                HostClass::Public
            }
        }
    }
}

fn classify_v4(ip: Ipv4Addr) -> HostClass {
    let o = ip.octets();
    if ip.is_loopback() {
        HostClass::Loopback
    } else if ip.is_private() {
        HostClass::PrivateRfc1918
    } else if ip.is_link_local()
        || ip.is_unspecified()
        || o[0] == 0 // 0.0.0.0/8 "this network"
        || o == [100, 100, 100, 200] // Alibaba Cloud metadata, inside the CGNAT block
        || o[..3] == [192, 0, 0] // 192.0.0.0/24 IETF protocol assignments
        || o[..3] == [192, 0, 2] // documentation
        || o[..3] == [198, 51, 100] // documentation
        || o[..3] == [203, 0, 113] // documentation
        || o[0] >= 224
    // multicast, reserved and broadcast
    {
        HostClass::LinkLocalOrMetadata
    } else if o[0] == 100 && o[1] & 0xc0 == 64 {
        HostClass::Cgnat // 100.64.0.0/10
    } else {
        HostClass::Public
    }
}

/// `host` without surrounding whitespace, IPv6 brackets or trailing dots.
fn trim_host(host: &str) -> &str {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    host.trim_end_matches('.')
}

/// The IP address `host` spells, in any form a URL parser accepts (`127.0.0.1`, `0x7f.1`,
/// `2130706433`, `[::1]`, `fe80::1%en0`); `None` for a domain name.
fn literal_ip(host: &str) -> Option<IpAddr> {
    let host = trim_host(host);
    if host.contains(':') {
        return host.split('%').next()?.parse().ok();
    }
    match Host::parse(host).ok()? {
        Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        Host::Domain(_) => None,
    }
}

/// The class of `host` when it can be known without DNS: an IP literal, `localhost`,
/// `*.localhost` or the GCP metadata name; `None` for any other name.
#[must_use]
pub fn classify_host_literal(host: &str) -> Option<HostClass> {
    if let Some(ip) = literal_ip(host) {
        return Some(classify_ip(ip));
    }
    let name = trim_host(host).to_ascii_lowercase();
    if name == "localhost" || name.ends_with(".localhost") {
        Some(HostClass::Loopback)
    } else if name == "metadata.google.internal" {
        Some(HostClass::LinkLocalOrMetadata)
    } else {
        None
    }
}

/// Resolves `host` and returns the addresses to connect to, refusing the host when it or any
/// address it resolves to is in a class that is not allowed. [`HostClass::LinkLocalOrMetadata`]
/// is never allowed; loopback, private and CGNAT addresses are allowed together by `allow_private`.
/// The returned class is the first non-public class found (`Public` when all are public).
///
/// # Errors
/// [`AppError::Validation`] for an empty or refused host, [`AppError::Network`] when DNS fails
/// or yields nothing.
pub async fn resolve_and_validate(
    host: &str,
    port: u16,
    allow_private: bool,
) -> Result<(HostClass, Vec<SocketAddr>), AppError> {
    resolve_with(host, port, allow_private, allow_private).await
}

/// [`resolve_and_validate`] with loopback allowed separately from private and CGNAT networks (the
/// agent's `fetch_url` allows neither in production; its tests allow loopback only).
///
/// # Errors
/// As [`resolve_and_validate`].
pub async fn resolve_with(
    host: &str,
    port: u16,
    allow_loopback: bool,
    allow_private: bool,
) -> Result<(HostClass, Vec<SocketAddr>), AppError> {
    let name = trim_host(host);
    if name.is_empty() {
        return Err(AppError::validation("the URL has no host"));
    }
    let permit = |class: HostClass, shown: &dyn std::fmt::Display| {
        let reason = match class {
            HostClass::Public => return Ok(()),
            HostClass::Loopback if allow_loopback => return Ok(()),
            HostClass::PrivateRfc1918 | HostClass::Cgnat if allow_private => return Ok(()),
            HostClass::Loopback => "loopback addresses are not allowed",
            HostClass::PrivateRfc1918 => "private network addresses are not allowed",
            HostClass::Cgnat => "carrier-grade NAT addresses are not allowed",
            HostClass::LinkLocalOrMetadata => {
                "link-local, cloud metadata, multicast, unspecified and reserved addresses are never allowed"
            }
        };
        Err(AppError::validation(format!(
            "refusing to connect to {shown}: {reason}"
        )))
    };
    if let Some(class) = classify_host_literal(name) {
        permit(class, &name)?;
        if let Some(ip) = literal_ip(name) {
            return Ok((class, vec![SocketAddr::new(ip, port)]));
        }
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((name, port))
        .await
        .map_err(|error| AppError::network(format!("could not resolve {name}: {error}")))?
        .collect();
    if addrs.is_empty() {
        return Err(AppError::network(format!("could not resolve {name}")));
    }
    let mut worst = HostClass::Public;
    for addr in &addrs {
        let class = classify_ip(addr.ip());
        permit(class, &format_args!("{name} ({})", addr.ip()))?;
        if class != HostClass::Public {
            worst = class;
        }
    }
    Ok((worst, addrs))
}

/// Whether `host` is covered by a `no_proxy` list, i.e. must be reached directly. Matching is
/// case-insensitive; an entry is `*` (everything), an IP address, a CIDR block (`10.0.0.0/8`),
/// `*.suffix` / `.suffix` (subdomains only) or a bare name (that host and its subdomains).
/// Names never match an IP-literal host, and blank entries are ignored.
#[must_use]
pub fn host_allowed_by_no_proxy(host: &str, patterns: &[String]) -> bool {
    let host = trim_host(host).to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    let host_ip = literal_ip(&host);
    patterns.iter().any(|pattern| {
        let pattern = trim_host(&pattern.to_ascii_lowercase()).to_owned();
        if pattern.is_empty() {
            return false;
        }
        if pattern == "*" {
            return true;
        }
        if let Some((net, prefix)) = pattern.split_once('/') {
            return match (host_ip, literal_ip(net), prefix.parse::<u32>()) {
                (Some(ip), Some(net), Ok(prefix)) => in_cidr(ip, net, prefix),
                _ => false,
            };
        }
        if let Some(pattern_ip) = literal_ip(&pattern) {
            return host_ip == Some(pattern_ip);
        }
        if host_ip.is_some() {
            return false;
        }
        match pattern
            .strip_prefix("*.")
            .or_else(|| pattern.strip_prefix('.'))
        {
            Some(suffix) => host
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.ends_with('.')),
            None => host
                .strip_suffix(pattern.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.ends_with('.')),
        }
    })
}

fn in_cidr(ip: IpAddr, net: IpAddr, prefix: u32) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) if prefix <= 32 => {
            let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) if prefix <= 128 => {
            let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

/// Whether `host` names a local-runtime machine: a loopback, private (RFC 1918, ULA) or CGNAT
/// literal, `localhost`, or a `*.local` (mDNS) name. The one definition shared by the proxy
/// routing of the LLM clients and the sniffer's host classes.
#[must_use]
pub fn is_local_host(host: &str) -> bool {
    match classify_host_literal(host) {
        Some(class) => matches!(
            class,
            HostClass::Loopback | HostClass::PrivateRfc1918 | HostClass::Cgnat
        ),
        None => trim_host(host).to_ascii_lowercase().ends_with(".local"),
    }
}

/// Whether traffic to `host` skips the global proxy: a local host ([`is_local_host`]) or one on
/// the `no_proxy` list.
#[must_use]
pub fn bypasses_global_proxy(host: &str, no_proxy: &[String]) -> bool {
    is_local_host(host) || host_allowed_by_no_proxy(host, no_proxy)
}

#[cfg(test)]
mod tests {
    use super::HostClass::{
        Cgnat, LinkLocalOrMetadata as Meta, Loopback, PrivateRfc1918 as Private, Public,
    };
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn every_address_range_has_its_class() {
        let table = [
            ("127.0.0.1", Loopback),
            ("127.255.255.254", Loopback),
            ("::1", Loopback),
            ("::ffff:127.0.0.1", Loopback),
            ("::127.0.0.1", Loopback),
            ("10.1.2.3", Private),
            ("172.16.0.1", Private),
            ("172.31.255.255", Private),
            ("192.168.1.1", Private),
            ("fc00::1", Private),
            ("fd12:3456::1", Private),
            ("fec0::1", Private),
            ("::ffff:10.0.0.1", Private),
            ("::192.168.0.1", Private),
            ("64:ff9b::a00:1", Private),
            ("169.254.169.254", Meta),
            ("169.254.0.0", Meta),
            ("169.254.255.255", Meta),
            ("100.64.0.1", Cgnat),
            ("100.64.0.0", Cgnat),
            ("100.99.99.99", Cgnat),
            ("100.100.100.199", Cgnat),
            ("100.100.100.201", Cgnat),
            ("100.127.255.255", Cgnat),
            ("::ffff:100.64.0.1", Cgnat),
            ("64:ff9b::6440:1", Cgnat),
            ("100.100.100.200", Meta),
            ("::ffff:100.100.100.200", Meta),
            ("fd00:ec2::254", Meta),
            ("fd00:ec2::255", Private),
            ("fd00::1", Private),
            ("192.0.0.1", Meta),
            ("192.0.2.1", Meta),
            ("198.51.100.7", Meta),
            ("203.0.113.9", Meta),
            ("::ffff:203.0.113.9", Meta),
            ("2001:db8::1", Meta),
            ("2001:db8:ffff::1", Meta),
            ("100::1", Meta),
            ("192.0.1.1", Public),
            ("198.17.255.255", Public),
            ("198.18.0.1", Public),
            ("198.19.255.255", Public),
            ("198.20.0.1", Public),
            ("2001:db9::1", Public),
            ("0.0.0.0", Meta),
            ("0.1.2.3", Meta),
            ("224.0.0.1", Meta),
            ("239.255.255.250", Meta),
            ("255.255.255.255", Meta),
            ("240.0.0.1", Meta),
            ("::", Meta),
            ("fe80::1", Meta),
            ("febf::1", Meta),
            ("ff02::1", Meta),
            ("::ffff:169.254.169.254", Meta),
            ("::ffff:0.0.0.0", Meta),
            ("64:ff9b::a9fe:a9fe", Meta),
            ("8.8.8.8", Public),
            ("1.1.1.1", Public),
            ("172.15.255.255", Public),
            ("172.32.0.1", Public),
            ("100.63.255.255", Public),
            ("100.128.0.1", Public),
            ("169.253.0.1", Public),
            ("169.255.0.1", Public),
            ("223.255.255.255", Public),
            ("2606:4700:4700::1111", Public),
            ("::ffff:8.8.8.8", Public),
            ("64:ff9b::808:808", Public),
        ];
        for (text, class) in table {
            assert_eq!(classify_ip(ip(text)), class, "{text}");
        }
    }

    #[test]
    fn transition_forms_take_the_class_of_the_embedded_address_or_are_refused() {
        let table = [
            // 6to4 2002::/16: the IPv4 address sits in bits 16..48.
            ("2002:7f00:1::", Loopback),
            ("2002:a9fe:a9fe::1", Meta),
            ("2002:a00:1::", Private),
            ("2002:6440:1::", Cgnat),
            ("2002:808:808::", Public),
            // SIIT ::ffff:0:a.b.c.d.
            ("::ffff:0:127.0.0.1", Loopback),
            ("::ffff:0:a9fe:a9fe", Meta),
            ("::ffff:0:10.0.0.1", Private),
            ("::ffff:0:8.8.8.8", Public),
            // Local-use NAT64 64:ff9b:1::/48 and Teredo 2001::/32 are reserved.
            ("64:ff9b:1::7f00:1", Meta),
            ("64:ff9b:1::808:808", Meta),
            ("64:ff9b:1:ffff:ffff:ffff:ffff:ffff", Meta),
            ("2001::1", Meta),
            ("2001:0:4136:e378:8000:63bf:3fff:fdd2", Meta),
            // Neighbours of those blocks stay public.
            ("2001:1::1", Public),
            ("64:ff9b:2::1", Public),
            ("2003:7f00:1::", Public),
        ];
        for (text, class) in table {
            assert_eq!(classify_ip(ip(text)), class, "{text}");
        }
    }

    #[test]
    fn host_literals_are_classified_without_dns() {
        let table = [
            ("127.0.0.1", Some(Loopback)),
            ("0x7f.1", Some(Loopback)),
            ("2130706433", Some(Loopback)),
            ("[::1]", Some(Loopback)),
            ("localhost", Some(Loopback)),
            ("LOCALHOST.", Some(Loopback)),
            ("api.localhost", Some(Loopback)),
            ("[::ffff:7f00:1]", Some(Loopback)),
            ("10.0.0.1", Some(Private)),
            ("[fd00::1]", Some(Private)),
            ("100.64.0.1", Some(Cgnat)),
            ("100.100.100.200", Some(Meta)),
            ("[fd00:ec2::254]", Some(Meta)),
            ("169.254.169.254", Some(Meta)),
            ("[fe80::1]", Some(Meta)),
            ("fe80::1%en0", Some(Meta)),
            ("0.0.0.0", Some(Meta)),
            ("metadata.google.internal", Some(Meta)),
            ("8.8.8.8", Some(Public)),
            ("example.com", None),
            ("notlocalhost", None),
            ("localhost.example.com", None),
            ("", None),
        ];
        for (host, class) in table {
            assert_eq!(classify_host_literal(host), class, "{host:?}");
        }
    }

    #[tokio::test]
    async fn always_refused_classes_stay_refused_even_when_private_hosts_are_allowed() {
        for host in [
            "169.254.169.254",
            "[fe80::1]",
            "224.0.0.1",
            "0.0.0.0",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "metadata.google.internal",
            "100.100.100.200",
            "::ffff:100.100.100.200",
            "[fd00:ec2::254]",
            "192.0.2.1",
            "[2001:db8::1]",
        ] {
            for allow in [false, true] {
                let error = resolve_and_validate(host, 80, allow).await.unwrap_err();
                assert!(
                    matches!(error, AppError::Validation { .. }),
                    "{host}: {error:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn loopback_private_and_cgnat_literals_need_allow_private() {
        for host in [
            "127.0.0.1",
            "[::1]",
            "10.0.0.1",
            "192.168.1.1",
            "fd00::1",
            "localhost",
            "100.64.0.1",
            "::ffff:100.64.0.1",
        ] {
            let error = resolve_and_validate(host, 80, false).await.unwrap_err();
            assert!(
                matches!(error, AppError::Validation { .. }),
                "{host}: {error:?}"
            );
        }
        let (class, addrs) = resolve_and_validate("10.0.0.1", 8080, true).await.unwrap();
        assert_eq!(class, Private);
        assert_eq!(addrs, [SocketAddr::new(ip("10.0.0.1"), 8080)]);
        let (class, addrs) = resolve_and_validate("100.64.0.1", 80, true).await.unwrap();
        assert_eq!(
            (class, addrs),
            (Cgnat, vec![SocketAddr::new(ip("100.64.0.1"), 80)])
        );
        let (class, addrs) = resolve_and_validate("[::1]", 80, true).await.unwrap();
        assert_eq!(
            (class, addrs),
            (Loopback, vec![SocketAddr::new(ip("::1"), 80)])
        );
        let (class, addrs) = resolve_and_validate("8.8.8.8", 443, false).await.unwrap();
        assert_eq!(
            (class, addrs),
            (Public, vec![SocketAddr::new(ip("8.8.8.8"), 443)])
        );
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_loopback_is_checked_after_dns() {
        let (class, addrs) = resolve_and_validate("localhost", 80, true).await.unwrap();
        assert_eq!(class, Loopback);
        assert!(!addrs.is_empty() && addrs.iter().all(|addr| addr.ip().is_loopback()));
    }

    #[tokio::test]
    async fn loopback_can_be_allowed_without_private() {
        assert!(resolve_with("127.0.0.1", 1, true, false).await.is_ok());
        assert!(resolve_with("10.0.0.1", 1, true, false).await.is_err());
        assert!(resolve_with("127.0.0.1", 1, false, true).await.is_err());
        assert!(resolve_with("  ", 1, true, true).await.is_err());
    }

    fn patterns(list: &[&str]) -> Vec<String> {
        list.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn no_proxy_matches_names_ips_cidrs_and_wildcards() {
        let defaults = patterns(&["localhost", "127.0.0.1", "::1", "*.local"]);
        for host in [
            "localhost",
            "LOCALHOST",
            "a.localhost",
            "127.0.0.1",
            "[::1]",
            "printer.local",
            "a.b.local",
        ] {
            assert!(host_allowed_by_no_proxy(host, &defaults), "{host}");
        }
        for host in [
            "local",
            "example.com",
            "notlocalhost",
            "127.0.0.2",
            "10.0.0.1",
            "",
        ] {
            assert!(!host_allowed_by_no_proxy(host, &defaults), "{host}");
        }
        let custom = patterns(&[
            " Example.COM ",
            ".corp.net",
            "10.0.0.0/8",
            "fd00::/8",
            "192.168.1.5",
            "",
        ]);
        for host in [
            "example.com",
            "www.example.com",
            "x.corp.net",
            "10.200.3.4",
            "[fd12::1]",
            "192.168.1.5",
        ] {
            assert!(host_allowed_by_no_proxy(host, &custom), "{host}");
        }
        for host in [
            "badexample.com",
            "corp.net",
            "11.0.0.1",
            "192.168.1.6",
            "fe80::1",
            "example.com.evil.org",
        ] {
            assert!(!host_allowed_by_no_proxy(host, &custom), "{host}");
        }
        assert!(host_allowed_by_no_proxy("anything.test", &patterns(&["*"])));
        assert!(!host_allowed_by_no_proxy("anything.test", &[]));
        // A name entry never matches an IP-literal host; a bad prefix matches nothing.
        assert!(!host_allowed_by_no_proxy("1.2.3.4", &patterns(&["3.4"])));
        assert!(!host_allowed_by_no_proxy(
            "10.0.0.1",
            &patterns(&["10.0.0.0/33", "10.0.0.0/x"])
        ));
        assert!(host_allowed_by_no_proxy(
            "1.2.3.4",
            &patterns(&["0.0.0.0/0"])
        ));
    }
}
