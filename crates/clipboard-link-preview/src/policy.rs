//! What this is allowed to connect to.
//!
//! A clipboard history is a list of places its owner has been. Fetching from
//! it turns every entry into a request somebody else can see, so the rules for
//! what may be contacted are written here, on their own, testable without a
//! network.
//!
//! The rule that matters most is the one about private addresses. Without it a
//! link preview becomes a scanner of the network the machine happens to be on:
//! a page can redirect to a router's admin panel, and the request would go out
//! from inside the firewall with the user's own credentials attached.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Why an address will not be contacted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// Not `http` or `https`.
    UnsupportedScheme,
    /// A name that resolves inside this machine or this network.
    PrivateHost,
    /// A name with no host at all.
    MissingHost,
}

/// Host names that never leave the machine, whatever DNS says.
const LOCAL_HOST_NAMES: [&str; 3] = ["localhost", "localhost.localdomain", "ip6-localhost"];

/// Suffixes reserved for local networks and for names that must not resolve.
const LOCAL_SUFFIXES: [&str; 6] = [
    ".local",
    ".localhost",
    ".internal",
    ".home.arpa",
    ".lan",
    ".localdomain",
];

/// Whether a scheme may be fetched at all.
pub fn scheme_is_fetchable(scheme: &str) -> bool {
    matches!(scheme, "http" | "https")
}

/// Whether a host name is worth resolving.
///
/// This is the cheap check that runs before any DNS lookup. It cannot catch a
/// public name that resolves to a private address — that is what
/// [`address_is_fetchable`] is for, and both are needed.
pub fn host_is_fetchable(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    if LOCAL_HOST_NAMES.contains(&host.as_str()) {
        return false;
    }
    if LOCAL_SUFFIXES.iter().any(|suffix| host.ends_with(suffix)) {
        return false;
    }
    // A bare address skips DNS, so it is judged as an address right here.
    if let Ok(address) = host.parse::<IpAddr>() {
        return address_is_fetchable(address);
    }
    // A name with no dot is a single label: a machine on the local network,
    // not a site.
    host.contains('.')
}

/// Whether an address may be connected to.
///
/// Applied to what DNS actually returned, after every redirect, because a name
/// that looked public a moment ago can answer with a private address the next
/// time it is asked.
pub fn address_is_fetchable(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => ipv4_is_fetchable(address),
        IpAddr::V6(address) => ipv6_is_fetchable(address),
    }
}

fn ipv4_is_fetchable(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    !(address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_multicast()
        || address.is_unspecified()
        || address.is_documentation()
        // 100.64.0.0/10, carrier-grade NAT.
        || (octets[0] == 100 && (64..128).contains(&octets[1]))
        // 192.0.0.0/24, IETF protocol assignments.
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        // 198.18.0.0/15, benchmarking.
        || (octets[0] == 198 && (18..20).contains(&octets[1]))
        // 240.0.0.0/4, reserved.
        || octets[0] >= 240)
}

fn ipv6_is_fetchable(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        // An IPv4 address wearing an IPv6 coat is still that address.
        return ipv4_is_fetchable(mapped);
    }
    let segments = address.segments();
    !(address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        // fc00::/7, unique local.
        || (segments[0] & 0xfe00) == 0xfc00
        // fe80::/10, link local.
        || (segments[0] & 0xffc0) == 0xfe80)
}

/// Decides whether a parsed URL may be fetched, before anything is sent.
pub fn url_is_fetchable(url: &url::Url) -> Result<(), Refusal> {
    if !scheme_is_fetchable(url.scheme()) {
        return Err(Refusal::UnsupportedScheme);
    }
    let Some(host) = url.host_str() else {
        return Err(Refusal::MissingHost);
    };
    if !host_is_fetchable(host) {
        return Err(Refusal::PrivateHost);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_web_schemes_are_fetched() {
        assert!(scheme_is_fetchable("http"));
        assert!(scheme_is_fetchable("https"));
        for scheme in ["file", "ftp", "data", "javascript", "mailto", "gopher"] {
            assert!(!scheme_is_fetchable(scheme), "{scheme}");
        }
    }

    #[test]
    fn nothing_on_this_machine_or_this_network_is_contacted() {
        for host in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "printer.local",
            "router.home.arpa",
            "nas.lan",
            "build-server",
            "127.0.0.1",
            "0.0.0.0",
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(!host_is_fetchable(host), "{host} must not be contacted");
        }
    }

    #[test]
    fn ordinary_public_names_and_addresses_are_allowed() {
        for host in [
            "example.com",
            "www.example.com",
            "8.8.8.8",
            "2606:4700::1111",
        ] {
            assert!(host_is_fetchable(host), "{host}");
        }
    }

    #[test]
    fn the_cloud_metadata_address_is_refused_as_an_address_too() {
        // 169.254.169.254 is the single most attacked target of this class of
        // bug, and it is reachable only from inside.
        assert!(!address_is_fetchable("169.254.169.254".parse().unwrap()));
        assert!(!address_is_fetchable("127.0.0.1".parse().unwrap()));
        assert!(address_is_fetchable("93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn a_url_is_judged_whole_before_anything_is_sent() {
        let allowed = url::Url::parse("https://example.com/page?a=1").unwrap();
        assert_eq!(url_is_fetchable(&allowed), Ok(()));

        for (raw, refusal) in [
            ("file:///etc/passwd", Refusal::UnsupportedScheme),
            ("http://192.168.0.1/admin", Refusal::PrivateHost),
            ("http://localhost:8080/", Refusal::PrivateHost),
        ] {
            let url = url::Url::parse(raw).unwrap();
            assert_eq!(url_is_fetchable(&url), Err(refusal), "{raw}");
        }
    }
}
