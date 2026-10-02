//! Working out "who is this visitor" without accounts.
//!
//! Voting is anonymous and limited per IP. Two details matter:
//!
//! 1. Behind a reverse proxy the TCP peer is the proxy, so the real client is
//!    in `X-Forwarded-For`. That header is trivially spoofable by the client,
//!    so we only trust the entries appended by *our own* proxies, counting
//!    from the right (see `TRUSTED_PROXY_HOPS`).
//! 2. A single IPv6 customer usually gets a whole /64 (2^64 addresses), so we
//!    treat the /64 prefix as "one network". Otherwise one person could vote
//!    billions of times.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{HeaderMap, request::Parts},
};
use sha2::{Digest, Sha256};

use crate::state::AppState;

/// The visitor's IP address, resolved according to the proxy configuration.
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip())
            .unwrap_or(IpAddr::from([127, 0, 0, 1]));
        Ok(ClientIp(resolve_client_ip(&parts.headers, peer, state.config.trusted_proxy_hops)))
    }
}

/// With `hops` trusted proxies, the client address is the `hops`-th entry from
/// the right of `X-Forwarded-For`. Anything further left was supplied by the
/// client and cannot be trusted.
///
/// The entry is picked *before* parsing: if entries that fail to parse were
/// dropped first, the index would shift left into client-controlled values.
/// If the chosen entry is missing or unparsable we use the TCP peer instead.
pub fn resolve_client_ip(headers: &HeaderMap, peer: IpAddr, hops: usize) -> IpAddr {
    if hops == 0 {
        return peer;
    }
    // Work on raw bytes so a non-UTF-8 value injected by the client can't make
    // a whole header line (including our proxy's entry) disappear.
    let entries: Vec<&[u8]> = headers
        .get_all("x-forwarded-for")
        .iter()
        .flat_map(|v| v.as_bytes().split(|b| *b == b','))
        .map(<[u8]>::trim_ascii)
        .filter(|e| !e.is_empty())
        .collect();
    if entries.len() < hops {
        return peer;
    }
    std::str::from_utf8(entries[entries.len() - hops]).ok().and_then(parse_forwarded_ip).unwrap_or(peer)
}

/// Accepts `1.2.3.4`, `1.2.3.4:5678`, `2001:db8::1`, `[2001:db8::1]` and
/// `[2001:db8::1]:5678` (some proxies include the client port).
fn parse_forwarded_ip(entry: &str) -> Option<IpAddr> {
    if let Ok(ip) = entry.parse() {
        return Some(ip);
    }
    if let Some(rest) = entry.strip_prefix('[') {
        return rest.split_once(']')?.0.parse().ok();
    }
    let (host, port) = entry.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    host.parse::<std::net::Ipv4Addr>().ok().map(IpAddr::V4)
}

/// Collapse an address to the unit we rate-limit on: the full IPv4 address,
/// or the /64 network for IPv6.
pub fn network_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                let prefix = Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0);
                format!("{prefix}/64")
            }
        },
    }
}

/// Keyed hash of the visitor's network. We store this instead of the raw IP so
/// the database never contains personal addresses.
pub fn hash_ip(secret: &str, ip: IpAddr) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update([0u8]);
    hasher.update(network_key(ip).as_bytes());
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(xff: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_str(xff).unwrap());
        h
    }

    #[test]
    fn ignores_forwarded_for_without_trusted_proxies() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        assert_eq!(resolve_client_ip(&headers("1.2.3.4"), peer, 0), peer);
    }

    #[test]
    fn takes_rightmost_entry_for_one_hop() {
        // The client sent a fake "6.6.6.6"; our proxy appended the real peer.
        let ip = resolve_client_ip(&headers("6.6.6.6, 203.0.113.9"), "10.0.0.1".parse().unwrap(), 1);
        assert_eq!(ip, "203.0.113.9".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn takes_nth_from_right_for_multiple_hops() {
        let ip = resolve_client_ip(&headers("6.6.6.6, 203.0.113.9, 10.1.1.1"), "10.0.0.1".parse().unwrap(), 2);
        assert_eq!(ip, "203.0.113.9".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn picks_entry_before_parsing() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        // Our proxy appended "ip:port"; the client-supplied 6.6.6.6 must not win.
        let ip = resolve_client_ip(&headers("6.6.6.6, 203.0.113.9:51234"), peer, 1);
        assert_eq!(ip, "203.0.113.9".parse::<IpAddr>().unwrap());
        let ip = resolve_client_ip(&headers("6.6.6.6, [2001:db8::7]:443"), peer, 1);
        assert_eq!(ip, "2001:db8::7".parse::<IpAddr>().unwrap());
        // Unparsable entry at the trusted position: fall back to the peer.
        assert_eq!(resolve_client_ip(&headers("6.6.6.6, unknown"), peer, 1), peer);
        // Fewer entries than trusted hops: misconfiguration, use the peer.
        assert_eq!(resolve_client_ip(&headers("6.6.6.6"), peer, 2), peer);
    }

    #[test]
    fn ipv6_is_grouped_by_64_prefix() {
        let a = network_key("2001:db8:1:2:aaaa::1".parse().unwrap());
        let b = network_key("2001:db8:1:2:bbbb::2".parse().unwrap());
        let c = network_key("2001:db8:1:3::1".parse().unwrap());
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a, "2001:db8:1:2::/64");
    }

    #[test]
    fn ipv4_mapped_addresses_are_treated_as_ipv4() {
        assert_eq!(network_key("::ffff:192.0.2.1".parse().unwrap()), "192.0.2.1");
    }

    #[test]
    fn hash_depends_on_secret() {
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        assert_ne!(hash_ip("a", ip), hash_ip("b", ip));
        assert_eq!(hash_ip("a", ip), hash_ip("a", ip));
    }
}
