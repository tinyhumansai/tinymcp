//! Finding an authorization server when a 401 names no protected-resource
//! metadata.
//!
//! A Bearer challenge without `resource_metadata` is not proof of a static
//! token. The 2025-06-18 authorization spec also publishes protected-resource
//! metadata at `/.well-known/oauth-protected-resource[/path]`, and servers on the
//! 2025-03-26 spec are their own authorization server, publishing RFC 8414
//! metadata on the MCP origin. This module looks in those places, in that
//! order, and nowhere else: default `/authorize` and `/token` paths are never
//! guessed.
//!
//! Every lookup is a `GET` to the MCP server's own origin over a client that
//! follows no redirects and reads at most [`MAX_DOCUMENT_BYTES`], so a server
//! cannot bounce discovery to another host or stall it with an endless body. An
//! authorization server named by protected-resource metadata is read the same
//! way the challenge path reads one.

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{StatusCode, Url};
use serde_json::Value;

use super::{McpHttpClient, fill_missing_metadata};
use crate::transport::redact_endpoint;
use tinymcp_bus::{AuthorizationServerMetadata, ProtectedResourceMetadata};

/// The total time a well-known lookup may take before it is abandoned.
pub(super) const DISCOVERY_BUDGET: Duration = Duration::from_secs(5);

/// The largest metadata document read; anything longer is ignored.
pub(super) const MAX_DOCUMENT_BYTES: usize = 64 * 1024;

const PROTECTED_RESOURCE_PATH: &str = "/.well-known/oauth-protected-resource";
const AUTHORIZATION_SERVER_PATH: &str = "/.well-known/oauth-authorization-server";
const OPENID_CONFIGURATION_PATH: &str = "/.well-known/openid-configuration";

/// What the well-known lookup concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WellKnownOutcome {
    /// Metadata naming at least one authorization server was found.
    Found(WellKnownAuthorization),
    /// Every place was checked and none published usable metadata.
    NotFound,
    /// A place could not be checked, so the answer may change on a retry.
    Transient,
}

/// Authorization metadata found on the server's origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WellKnownAuthorization {
    /// The well-known document that yielded the authorization server.
    pub(super) metadata_url: String,
    /// The protected-resource metadata, when the server published a valid one.
    pub(super) protected_resource_metadata: Option<ProtectedResourceMetadata>,
    /// Every authorization server that could be read.
    pub(super) authorization_servers: Vec<AuthorizationServerMetadata>,
}

impl WellKnownAuthorization {
    /// Whether some authorization server names both endpoints a sign-in needs.
    pub(super) fn offers_sign_in(&self) -> bool {
        self.authorization_servers.iter().any(|metadata| {
            metadata.authorization_endpoint.is_some() && metadata.token_endpoint.is_some()
        })
    }
}

/// One fetched discovery document.
#[derive(Debug)]
enum Fetched {
    Document(Value),
    Missing,
    Transient,
}

/// What the origin's own authorization-server metadata lookup found.
#[derive(Debug)]
enum OriginLookup {
    Found(String, AuthorizationServerMetadata),
    Missing,
    Transient,
}

impl McpHttpClient {
    /// Looks for authorization metadata on this server's origin, once per
    /// client.
    ///
    /// A definitive answer is cached; a transient one, including running out
    /// of [`DISCOVERY_BUDGET`], is not, so a later 401 looks again.
    pub(super) async fn well_known_authorization(&self) -> WellKnownOutcome {
        if let Some(cached) = self.well_known.lock().clone() {
            return cached;
        }

        let outcome = tokio::time::timeout(DISCOVERY_BUDGET, self.discover_from_well_known())
            .await
            .unwrap_or(WellKnownOutcome::Transient);

        if outcome != WellKnownOutcome::Transient {
            *self.well_known.lock() = Some(outcome.clone());
        }
        outcome
    }

    async fn discover_from_well_known(&self) -> WellKnownOutcome {
        let Some(origin) = origin_of(&self.endpoint) else {
            return WellKnownOutcome::NotFound;
        };
        let mut transient = false;
        let mut protected_resource = None;

        for candidate in protected_resource_candidates(&self.endpoint, &origin) {
            let document = match self.fetch_discovery_json(&candidate).await {
                Fetched::Document(document) => document,
                Fetched::Missing => continue,
                Fetched::Transient => {
                    transient = true;
                    continue;
                }
            };

            if let Some(metadata) = same_origin_protected_resource(&document, &origin) {
                let (servers, unreadable) = self
                    .read_authorization_servers(&metadata.authorization_servers)
                    .await;
                if !servers.is_empty() {
                    return WellKnownOutcome::Found(WellKnownAuthorization {
                        metadata_url: candidate,
                        protected_resource_metadata: Some(metadata),
                        authorization_servers: servers,
                    });
                }
                transient |= unreadable;
                protected_resource.get_or_insert((candidate, metadata));
                continue;
            }

            if let Some(metadata) = origin_authorization_server(&document, &origin) {
                return WellKnownOutcome::Found(WellKnownAuthorization {
                    metadata_url: candidate,
                    protected_resource_metadata: None,
                    authorization_servers: vec![metadata],
                });
            }

            tracing::debug!(
                url = %redact_endpoint(&candidate),
                "[mcp] ignoring a well-known document that is neither same-origin resource nor issuer metadata"
            );
        }

        match self.origin_authorization_server(&origin).await {
            OriginLookup::Found(metadata_url, metadata) => {
                let (metadata_url, protected_resource_metadata) = match protected_resource {
                    Some((url, resource)) => (url, Some(resource)),
                    None => (metadata_url, None),
                };
                WellKnownOutcome::Found(WellKnownAuthorization {
                    metadata_url,
                    protected_resource_metadata,
                    authorization_servers: vec![metadata],
                })
            }
            OriginLookup::Transient => WellKnownOutcome::Transient,
            OriginLookup::Missing if transient => WellKnownOutcome::Transient,
            OriginLookup::Missing => WellKnownOutcome::NotFound,
        }
    }

    /// Reads each authorization server a protected resource names.
    ///
    /// Returns those that could be read, and whether any could not.
    async fn read_authorization_servers(
        &self,
        issuers: &[String],
    ) -> (Vec<AuthorizationServerMetadata>, bool) {
        let mut servers = Vec::new();
        let mut unreadable = false;
        for issuer in issuers {
            match self.fetch_authorization_server_metadata(issuer).await {
                Ok(metadata) => servers.push(metadata),
                Err(error) => {
                    unreadable = true;
                    tracing::debug!(
                        issuer = %redact_endpoint(issuer),
                        "[mcp] skipping an authorization server whose metadata could not be read: {error}"
                    );
                }
            }
        }
        (servers, unreadable)
    }

    /// Reads RFC 8414 metadata from the origin, then `OpenID` discovery, and
    /// accepts a document only when its issuer is the origin itself.
    async fn origin_authorization_server(&self, origin: &Url) -> OriginLookup {
        let base = origin_text(origin);
        let mut transient = false;
        let mut found: Option<(String, AuthorizationServerMetadata)> = None;

        for path in [AUTHORIZATION_SERVER_PATH, OPENID_CONFIGURATION_PATH] {
            let url = format!("{base}{path}");
            let document = match self.fetch_discovery_json(&url).await {
                Fetched::Document(document) => document,
                Fetched::Missing => continue,
                Fetched::Transient => {
                    transient = true;
                    continue;
                }
            };
            let Some(metadata) = origin_authorization_server(&document, origin) else {
                continue;
            };
            found = Some(match found {
                None => (url, metadata),
                Some((first_url, first)) => (first_url, fill_missing_metadata(first, metadata)),
            });
            if found.as_ref().is_some_and(|(_, metadata)| {
                metadata.authorization_endpoint.is_some() && metadata.token_endpoint.is_some()
            }) {
                break;
            }
        }

        match found {
            Some((url, metadata)) => OriginLookup::Found(url, metadata),
            None if transient => OriginLookup::Transient,
            None => OriginLookup::Missing,
        }
    }

    /// Fetches one discovery document without following redirects.
    ///
    /// A redirect, a 401, 403, 404 or 410, a body that is not JSON and a body
    /// over [`MAX_DOCUMENT_BYTES`] all mean the document is not there. A 5xx or
    /// a transport failure means it could not be checked.
    async fn fetch_discovery_json(&self, url: &str) -> Fetched {
        let client = match self.discovery_client_for(url, &self.discovery_http).await {
            Ok(client) => client,
            Err(error) => {
                tracing::debug!(url = %redact_endpoint(url), "[mcp] unsafe discovery URL refused: {error}");
                return Fetched::Missing;
            }
        };
        let response = match client.get(url).send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(url = %redact_endpoint(url), "[mcp] well-known lookup failed: {error}");
                return Fetched::Transient;
            }
        };

        let status = response.status();
        if status.is_server_error() {
            return Fetched::Transient;
        }
        if !status.is_success() {
            if !(status.is_redirection() || is_definitive_absence(status)) {
                tracing::debug!(url = %redact_endpoint(url), %status, "[mcp] unexpected well-known status");
            }
            return Fetched::Missing;
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_DOCUMENT_BYTES as u64)
        {
            return Fetched::Missing;
        }

        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                return Fetched::Transient;
            };
            if body.len() + chunk.len() > MAX_DOCUMENT_BYTES {
                return Fetched::Missing;
            }
            body.extend_from_slice(&chunk);
        }

        serde_json::from_slice(&body).map_or(Fetched::Missing, Fetched::Document)
    }
}

fn is_definitive_absence(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::GONE
    )
}

/// The scheme, host and port of `endpoint`, with an empty path.
fn origin_of(endpoint: &str) -> Option<Url> {
    let url = Url::parse(endpoint).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return None;
    }
    Url::parse(&url.origin().ascii_serialization()).ok()
}

/// `origin` without its trailing slash, ready to have a path appended.
fn origin_text(origin: &Url) -> String {
    origin.as_str().trim_end_matches('/').to_string()
}

/// Where protected-resource metadata may live: under the endpoint's path
/// first, then at the origin root.
fn protected_resource_candidates(endpoint: &str, origin: &Url) -> Vec<String> {
    let base = origin_text(origin);
    let root = format!("{base}{PROTECTED_RESOURCE_PATH}");
    let path = Url::parse(endpoint)
        .map(|url| url.path().trim_end_matches('/').to_string())
        .unwrap_or_default();

    if path.is_empty() {
        vec![root]
    } else {
        vec![format!("{root}{path}"), root]
    }
}

/// `document` read as protected-resource metadata for a resource on `origin`.
fn same_origin_protected_resource(
    document: &Value,
    origin: &Url,
) -> Option<ProtectedResourceMetadata> {
    let metadata: ProtectedResourceMetadata = serde_json::from_value(document.clone()).ok()?;
    let resource = Url::parse(&metadata.resource).ok()?;
    (resource.origin() == origin.origin()).then_some(metadata)
}

/// `document` read as authorization-server metadata whose issuer is `origin`.
fn origin_authorization_server(
    document: &Value,
    origin: &Url,
) -> Option<AuthorizationServerMetadata> {
    let metadata: AuthorizationServerMetadata = serde_json::from_value(document.clone()).ok()?;
    issuer_is_origin(&metadata.issuer, origin).then_some(metadata)
}

/// Whether `issuer` names exactly `origin`, with or without one trailing
/// slash.
///
/// An origin URL has no path to distinguish, so `https://a.example` and
/// `https://a.example/` identify the same issuer. Anything longer does not.
pub(super) fn issuer_is_origin(issuer: &str, origin: &Url) -> bool {
    let base = origin_text(origin);
    issuer == base || issuer.strip_suffix('/') == Some(base.as_str())
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
