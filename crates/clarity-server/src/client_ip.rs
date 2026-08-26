use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

/// Resolves the rate-limit identity through an explicitly trusted proxy chain.
///
/// `trusted_proxy_hops == 0` deliberately ignores forwarding headers. For a
/// proxied deployment, the right-most forwarded address before the configured
/// number of trusted hops is the client. Malformed or incomplete chains fail
/// closed to the transport peer, preventing an attacker from choosing a rate
/// limit key with a forged header.
#[must_use]
pub fn client_ip(
    remote: SocketAddr,
    headers: &HeaderMap,
    trusted_proxy_hops: usize,
) -> IpAddr {
    if trusted_proxy_hops == 0 {
        return remote.ip();
    }
    let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    else {
        return remote.ip();
    };
    let Some(forwarded) = value
        .split(',')
        .map(str::trim)
        .map(str::parse::<IpAddr>)
        .collect::<Result<Vec<_>, _>>()
        .ok()
    else {
        return remote.ip();
    };
    forwarded
        .len()
        .checked_sub(trusted_proxy_hops)
        .and_then(|index| forwarded.get(index))
        .copied()
        .unwrap_or_else(|| remote.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote() -> SocketAddr {
        "172.20.0.3:443".parse().expect("valid address")
    }

    fn headers(forwarded_for: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            forwarded_for.parse().expect("valid header"),
        );
        headers
    }

    #[test]
    fn direct_deployments_ignore_forwarding_headers() {
        assert_eq!(
            client_ip(remote(), &headers("198.51.100.7"), 0),
            remote().ip()
        );
    }

    #[test]
    fn one_trusted_proxy_selects_its_immediate_client() {
        assert_eq!(
            client_ip(remote(), &headers("192.0.2.8, 198.51.100.7"), 1),
            "198.51.100.7".parse::<IpAddr>().expect("valid address")
        );
    }

    #[test]
    fn multiple_trusted_proxies_walk_back_the_chain() {
        assert_eq!(
            client_ip(remote(), &headers("192.0.2.8, 198.51.100.7"), 2),
            "192.0.2.8".parse::<IpAddr>().expect("valid address")
        );
    }

    #[test]
    fn malformed_or_short_chains_fall_back_to_the_transport_peer() {
        assert_eq!(client_ip(remote(), &headers("not-an-ip"), 1), remote().ip());
        assert_eq!(
            client_ip(remote(), &headers("198.51.100.7"), 2),
            remote().ip()
        );
    }
}
