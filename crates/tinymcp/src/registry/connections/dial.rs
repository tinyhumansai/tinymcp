//! Working out what to dial, and with which credentials.

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::Url;

use tinymcp_bus::{HttpHeader, McpAuthConfig};

/// The prefix marking a stored credential as internal bookkeeping.
///
/// Keys beginning this way are never sent to a server. The OAuth refresh bundle
/// is the one that matters: it holds a refresh token and a client secret, and
/// putting either in a request header would hand a server credentials it has no
/// business seeing.
const INTERNAL_KEY_PREFIX: &str = "__";

/// How long to spend resolving redirects before giving up and dialling the
/// original.
const REDIRECT_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(10);

/// How many redirects to follow while resolving.
const MAX_REDIRECTS: usize = 5;

/// Builds request credentials from a server's stored values.
///
/// Every stored name is a header name and its value the secret, which is how
/// the registries describe remote authentication. All of them are applied: a
/// server wanting a client key *and* a client secret gets both, not the first.
///
/// Internal keys are skipped, as are blank values. Nothing usable yields
/// [`McpAuthConfig::None`], which is the right state for an OAuth-only server
/// that has not been signed into — its 401 then surfaces the challenge.
pub(crate) fn build_http_auth(env: &BTreeMap<String, String>) -> McpAuthConfig {
    let headers: Vec<HttpHeader> = env
        .iter()
        .filter(|(name, value)| !name.starts_with(INTERNAL_KEY_PREFIX) && !value.trim().is_empty())
        .map(|(name, value)| HttpHeader::new(name, value))
        .collect();

    match headers.len() {
        0 => McpAuthConfig::None,
        // One header keeps the simpler variant, which is what the wire form
        // looked like before multi-header servers existed.
        1 => headers
            .into_iter()
            .next()
            .map_or(McpAuthConfig::None, |header| McpAuthConfig::Header {
                name: header.name,
                value: header.value,
            }),
        _ => McpAuthConfig::Headers { headers },
    }
}

/// Whether a stored credential name is internal bookkeeping.
#[must_use]
pub(super) fn is_internal_key(name: &str) -> bool {
    name.starts_with(INTERNAL_KEY_PREFIX)
}

/// Follows redirects unauthenticated and reports where they end.
///
/// # Why resolve at all
///
/// HTTP clients strip `Authorization` across a cross-origin redirect, which is
/// the right default. But servers are commonly published behind a vanity host
/// that redirects to the real endpoint, and the token then never arrives.
/// Resolving first means the authenticated request goes straight to the final
/// address with no redirect left to strip it.
///
/// The final status is irrelevant — an unauthenticated probe of a real endpoint
/// usually answers 401 or 405. Only the address it settled on is read.
///
/// Returns `None` on any failure, and the caller falls back to the original. A
/// server that does not redirect resolves to itself.
pub(super) async fn resolve_final_url(url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(REDIRECT_RESOLUTION_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .build()
        .ok()?;

    match client.get(url).send().await {
        Ok(response) => Some(response.url().to_string()),
        Err(error) => {
            tracing::debug!(
                endpoint = %crate::redact_endpoint(url),
                "could not resolve redirects: {error}"
            );
            None
        }
    }
}

/// Decides which address is safe to send stored credentials to.
///
/// A same-origin redirect is always honored. A cross-origin one is honored only
/// when it lands on HTTPS: transport security authenticates the host it arrived
/// at, so the credential is not handed to whoever answered on a cleartext port.
///
/// Otherwise the original address is dialled, where the HTTP client's own
/// cross-origin stripping protects the credential — the request may fail, but
/// it fails without giving anything away.
///
/// # What this does not defend against
///
/// A compromised host that redirects to *another HTTPS origin* still receives
/// the credential. Pinning the resolved origin at install time would close that,
/// and is the natural next step; this stops the cleartext and downgrade cases,
/// which are the ones a passive network attacker can cause.
#[must_use]
pub(super) fn credential_safe_dial_url(original: &str, resolved: String) -> String {
    let (Ok(from), Ok(to)) = (Url::parse(original), Url::parse(&resolved)) else {
        return original.to_string();
    };

    let same_origin = from.scheme() == to.scheme()
        && from.host_str() == to.host_str()
        && from.port_or_known_default() == to.port_or_known_default();

    if same_origin || to.scheme() == "https" {
        return resolved;
    }

    tracing::warn!(
        from = %crate::redact_endpoint(original),
        to = %crate::redact_endpoint(&resolved),
        "refusing to send stored credentials to a cleartext cross-origin redirect; \
         dialling the original address instead"
    );
    original.to_string()
}
