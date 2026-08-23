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
    /// A port other than the two the web is served on.
    UnsupportedPort,
}

/// The only ports this will connect to.
///
/// A clipboard entry is untrusted text, and an address may name any port it
/// likes. Following that turns a link preview into a way to knock on every
/// service a host runs — the request would come from this machine, and the
/// answer, even just "connected" or "refused", is information the page's author
/// should not be able to collect.
const FETCHABLE_PORTS: [u16; 2] = [80, 443];

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
        // 0.0.0.0/8, "this network" — `is_unspecified` covers only 0.0.0.0
        // itself, and the rest of the block reaches the local host on some
        // stacks.
        || octets[0] == 0
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
    // An IPv4 address wearing an IPv6 coat is still that address, in every one
    // of the several coats IPv6 provides. `to_ipv4` covers both the mapped
    // form (`::ffff:a.b.c.d`) and the deprecated compatible one (`::a.b.c.d`),
    // which some stacks still route.
    if let Some(embedded) = address.to_ipv4() {
        return ipv4_is_fetchable(embedded);
    }
    let segments = address.segments();
    if let Some(embedded) = embedded_ipv4(address) {
        // 6to4 and Teredo carry an IPv4 address inside them, so an address
        // inside this network can be reached through either.
        return ipv4_is_fetchable(embedded);
    }
    !(address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        // fc00::/7, unique local.
        || (segments[0] & 0xfe00) == 0xfc00
        // fe80::/10, link local.
        || (segments[0] & 0xffc0) == 0xfe80
        // 2001:db8::/32, documentation.
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        // 64:ff9b::/96, NAT64 — the low bits are an IPv4 address, and the
        // `to_ipv4` above does not recognise this prefix.
        || (segments[0] == 0x0064 && segments[1] == 0xff9b))
}

/// The IPv4 address a transition-mechanism IPv6 address carries, if any.
fn embedded_ipv4(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = address.segments();
    match segments[0] {
        // 2002::/16, 6to4: the address is in the next two segments.
        0x2002 => Some(Ipv4Addr::new(
            (segments[1] >> 8) as u8,
            segments[1] as u8,
            (segments[2] >> 8) as u8,
            segments[2] as u8,
        )),
        // 2001:0::/32, Teredo: the client address is the last two segments,
        // stored inverted.
        0x2001 if segments[1] == 0 => {
            let high = !segments[6];
            let low = !segments[7];
            Some(Ipv4Addr::new(
                (high >> 8) as u8,
                high as u8,
                (low >> 8) as u8,
                low as u8,
            ))
        }
        // 64:ff9b::/96, NAT64.
        0x0064 if segments[1] == 0xff9b => Some(Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        )),
        _ => None,
    }
}

/// Whether a port may be connected to.
pub fn port_is_fetchable(port: u16) -> bool {
    FETCHABLE_PORTS.contains(&port)
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
    let Some(port) = url.port_or_known_default() else {
        return Err(Refusal::UnsupportedPort);
    };
    if !port_is_fetchable(port) {
        return Err(Refusal::UnsupportedPort);
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
    fn the_zeroth_network_is_refused_whole_rather_than_only_its_first_address() {
        // `is_unspecified` matches 0.0.0.0 alone; the rest of 0.0.0.0/8 still
        // reaches the local host on several stacks.
        for address in ["0.0.0.0", "0.0.0.1", "0.1.2.3", "0.255.255.255"] {
            assert!(
                !address_is_fetchable(address.parse().unwrap()),
                "{address} must not be contacted"
            );
        }
    }

    #[test]
    fn an_ipv4_address_hidden_inside_ipv6_is_judged_as_that_address() {
        for address in [
            // Mapped.
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            // Compatible, deprecated but still routed by some stacks.
            "::127.0.0.1",
            "::192.168.1.1",
            // 6to4 carrying a private address.
            "2002:c0a8:0101::1",
            // NAT64 carrying a loopback address.
            "64:ff9b::7f00:1",
        ] {
            assert!(
                !address_is_fetchable(address.parse().unwrap()),
                "{address} smuggles a private address"
            );
        }
        // A 6to4 address carrying a public one stays reachable.
        assert!(address_is_fetchable("2002:5db8:d822::1".parse().unwrap()));
    }

    #[test]
    fn only_the_ports_the_web_is_served_on_are_contacted() {
        // Otherwise a clipboard entry becomes a way to knock on every service
        // this machine can reach, and to learn which ones answered.
        assert!(port_is_fetchable(80));
        assert!(port_is_fetchable(443));
        for port in [22, 25, 3306, 5432, 6379, 8080, 9200, 11211] {
            assert!(!port_is_fetchable(port), "{port}");
        }

        for raw in [
            "http://example.com:22/",
            "https://example.com:8080/",
            "http://example.com:5432/",
        ] {
            let url = url::Url::parse(raw).unwrap();
            assert_eq!(
                url_is_fetchable(&url),
                Err(Refusal::UnsupportedPort),
                "{raw}"
            );
        }
        // The defaults are written or implied, and both pass.
        for raw in ["https://example.com/", "http://example.com:80/page"] {
            assert_eq!(
                url_is_fetchable(&url::Url::parse(raw).unwrap()),
                Ok(()),
                "{raw}"
            );
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
