//! Refusing internal targets among the endpoints a server advertises.
//!
//! The authorization, registration and token endpoints come from the server
//! being signed in to, and the flow then POSTs to them **from the host**. An
//! unchecked endpoint is a server-side request-forgery primitive: a hostile
//! server can aim the flow at an internal service or the cloud metadata
//! address. A host serving untrusted servers turns this guard on with
//! [`OAuthFlow::require_public_endpoints`](super::OAuthFlow::require_public_endpoints).
//! It is off by default because a desktop host legitimately signs in to
//! loopback development servers.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use reqwest::Url;

use crate::error::{Error, Result};

/// Refuses `raw` unless it is `https` and every address its host resolves to
/// is public. `what` names the endpoint in the error.
///
/// A single blocked record is enough to refuse. The returned addresses can be
/// pinned to the HTTP client so the eventual connection uses the vetted result
/// instead of resolving the name again. Resolution runs on the blocking pool
/// so a slow resolver cannot stall the executor.
///
/// # Errors
///
/// [`Error::MalformedResponse`] when the URL is unsafe, or
/// [`Error::EndpointResolution`] when DNS resolution could not complete.
pub(crate) async fn guard_endpoint(raw: &str, what: &str) -> Result<Vec<SocketAddr>> {
    let refuse = |why: String| Error::malformed(format!("{what} endpoint refused: {why}"));
    let url = Url::parse(raw).map_err(|error| refuse(format!("not a valid url: {error}")))?;
    if url.scheme() != "https" {
        return Err(refuse(format!("must use https, not `{}`", url.scheme())));
    }
    let host = url
        .host_str()
        .ok_or_else(|| refuse("it has no host".to_string()))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = url.port_or_known_default().unwrap_or(443);

    let addresses: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        let resolution = tokio::task::spawn_blocking(move || {
            (host.as_str(), port)
                .to_socket_addrs()
                .map(Iterator::collect::<Vec<_>>)
        })
        .await
        .map_err(|error| Error::EndpointResolution {
            what: what.to_string(),
            detail: format!("resolution did not finish: {error}"),
        })?;
        checked_addresses(what, resolution)?
    };
    if addresses.iter().any(|addr| is_blocked_ip(&addr.ip())) {
        return Err(refuse("it resolves to a disallowed address".to_string()));
    }
    Ok(addresses)
}

/// Treat resolver outages and an empty DNS answer as retryable failures.
fn checked_addresses(
    what: &str,
    addresses: std::io::Result<Vec<SocketAddr>>,
) -> Result<Vec<SocketAddr>> {
    let addresses = addresses.map_err(|error| Error::EndpointResolution {
        what: what.to_string(),
        detail: error.to_string(),
    })?;
    if addresses.is_empty() {
        return Err(Error::EndpointResolution {
            what: what.to_string(),
            detail: "the host has no addresses".to_string(),
        });
    }
    Ok(addresses)
}

/// Builds an HTTP client pinned to the public addresses checked for `raw`.
/// Redirects are disabled so a validated endpoint cannot replay credentials to
/// a different, unchecked destination.
pub(crate) async fn guarded_client(raw: &str, what: &str) -> Result<reqwest::Client> {
    let addresses = guard_endpoint(raw, what).await?;
    let url = Url::parse(raw)
        .map_err(|error| Error::malformed(format!("invalid {what} url: {error}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::malformed(format!("{what} endpoint has no host")))?
        .trim_start_matches('[')
        .trim_end_matches(']');
    client_pinned_to(host, &addresses)
}

fn client_pinned_to(host: &str, addresses: &[SocketAddr]) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    if host.parse::<IpAddr>().is_err() {
        builder = builder.resolve_to_addrs(host, addresses);
    }
    builder.build().map_err(|source| Error::ClientBuild {
        source: Box::new(source.without_url()),
    })
}

/// Whether an address is one the flow must never POST OAuth material to:
/// loopback, private or unique-local, link-local (the metadata address among
/// them), unspecified, broadcast, documentation, multicast, or `0.0.0.0/8`.
pub(crate) fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || v4.octets()[0] == 0
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
                || (v4.octets()[0] == 198 && (v4.octets()[1] & 0xfe) == 18)
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4() {
                return is_blocked_ip(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || (v6.segments()[0] == 0x0064 && v6.segments()[1] == 0xff9b)
                || (v6.segments()[0] & 0xffc0) == 0xfec0
        }
    }
}

#[cfg(test)]
#[path = "endpoint_guard_tests.rs"]
mod tests;
