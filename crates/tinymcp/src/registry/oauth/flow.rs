//! Running the authorization-code flow.

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine as _;
use parking_lot::Mutex;
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::tokens::{persist, post_form};
use super::types::{AuthDetection, PendingAuthorization, TokenResponse};
use crate::error::{Error, Result};
use crate::registry::Store;
use crate::transport::http::McpHttpClient;
use tinymcp_bus::{McpProxyConfig, Transport};

/// URL-safe base64 without padding, which is what OAuth uses throughout.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// How long an unfinished authorization stays parked.
///
/// A user who starts a sign-in and closes the tab leaves an entry behind
/// holding a client secret. Ten minutes is longer than any real sign-in and
/// short enough that abandoned ones do not accumulate.
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

/// How long to wait on an authorization server.
const AUTH_SERVER_TIMEOUT_SECS: u64 = 20;

/// How long to wait when probing a server for what it wants.
const PROBE_TIMEOUT_SECS: u64 = 20;

/// The client name presented during dynamic registration.
///
/// A user sees this on the consent screen, so it names the library rather than
/// leaving it blank. A host that wants its own name there registers its own
/// client; that is a larger change than a string.
const CLIENT_NAME: &str = "TinyMCP";

/// Runs browser sign-in for HTTP-remote servers.
///
/// Holds the authorizations parked between the redirect out and back. One
/// instance per host; two would not see each other's pending state, and a
/// redirect would come back to the wrong one.
#[derive(Debug)]
pub struct OAuthFlow {
    http: reqwest::Client,
    proxy: Option<McpProxyConfig>,
    pending: Mutex<HashMap<String, PendingAuthorization>>,
}

impl OAuthFlow {
    /// Builds a flow that dials through `proxy`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when the HTTP client cannot be built.
    pub fn new(proxy: Option<McpProxyConfig>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(AUTH_SERVER_TIMEOUT_SECS))
            .build()
            .map_err(|source| Error::ClientBuild {
                source: Box::new(source.without_url()),
            })?;

        Ok(Self {
            http,
            proxy,
            pending: Mutex::new(HashMap::new()),
        })
    }

    /// The HTTP client this flow uses, for the refresh path.
    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Classifies what a server wants before it will talk.
    ///
    /// Decided by probing, not by reading registry metadata, which is often
    /// wrong about this. A server that answers without a challenge is open; one
    /// that challenges with a usable authorization endpoint wants a browser
    /// sign-in; anything else wants a static token.
    ///
    /// A discovery failure reports a static token rather than an error. The
    /// user can paste one and find out, which beats being blocked by a probe
    /// that could not parse an unusual challenge.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when there is no such install, and
    /// [`Error::Store`] when it cannot be read. A store failure is *not*
    /// collapsed into "open": reporting a server as needing nothing when the
    /// lookup failed would show the user a state that was never checked.
    pub async fn detect(&self, store: &Store, server_id: &str) -> Result<AuthDetection> {
        let Some(url) = self.remote_url(store, server_id)? else {
            // A subprocess install has no HTTP authorization to discover.
            return Ok(AuthDetection::open());
        };

        let client = McpHttpClient::builder(url)
            .timeout_secs(PROBE_TIMEOUT_SECS)
            .proxy(self.proxy.clone())
            .build()?;

        match client.discover_authorization().await {
            Ok(None) => Ok(AuthDetection::open()),
            Ok(Some(context)) => Ok(context
                .authorization_server_metadata
                .iter()
                .find(|metadata| {
                    metadata.authorization_endpoint.is_some() && supports_authorization_code(metadata)
                })
                .and_then(|metadata| {
                    metadata.authorization_endpoint.clone().map(|endpoint| {
                        AuthDetection::oauth(endpoint, metadata.grant_types_supported.clone())
                    })
                })
                .unwrap_or_else(AuthDetection::static_token)),
            Err(error) => {
                tracing::debug!(
                    server_id,
                    "discovery failed; offering a static token instead: {error}"
                );
                Ok(AuthDetection::static_token())
            }
        }
    }

    /// Starts a sign-in and returns the URL to open in a browser.
    ///
    /// `redirect_uri` is the loopback address the host is listening on. It is a
    /// parameter because only the host knows which port it actually bound — see
    /// the module note.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AuthDiscovery`] when no advertised authorization server
    /// offers everything the flow needs, [`Error::MalformedResponse`] when
    /// registration answers with something unusable, plus whatever the
    /// transport returns.
    pub async fn begin(
        &self,
        store: &Store,
        server_id: &str,
        redirect_uri: &str,
    ) -> Result<String> {
        let url = self
            .remote_url(store, server_id)?
            .ok_or_else(|| Error::malformed("oauth applies only to http-remote servers"))?;

        let client = McpHttpClient::builder(url.clone())
            .timeout_secs(PROBE_TIMEOUT_SECS)
            .proxy(self.proxy.clone())
            .build()?;

        let context = client
            .discover_authorization()
            .await?
            .ok_or_else(|| Error::malformed("the server does not require authorization"))?;

        // Chosen by capability rather than by position: the first advertised
        // authorization server may be incomplete while a later one is usable.
        let metadata = context
            .authorization_server_metadata
            .iter()
            .find(|metadata| {
                metadata.authorization_endpoint.is_some()
                    && metadata.token_endpoint.is_some()
                    && metadata.registration_endpoint.is_some()
                    && supports_authorization_code(metadata)
            })
            .ok_or_else(|| Error::AuthDiscovery {
                detail: "no advertised authorization server offers an authorize endpoint, a token \
                         endpoint, and dynamic client registration together"
                    .to_string(),
                challenge: Box::new(context.challenge.clone()),
            })?;

        // Each was checked present by the search above.
        let (Some(authorization_endpoint), Some(token_endpoint), Some(registration_endpoint)) = (
            metadata.authorization_endpoint.clone(),
            metadata.token_endpoint.clone(),
            metadata.registration_endpoint.clone(),
        ) else {
            return Err(Error::malformed(
                "the chosen authorization server lost an endpoint between checks",
            ));
        };

        let (client_id, client_secret) = self
            .register_client(&registration_endpoint, redirect_uri)
            .await?;

        let (code_verifier, code_challenge) = generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();

        {
            let mut pending = self.pending.lock();
            prune_expired(&mut pending);
            pending.insert(
                state.clone(),
                PendingAuthorization {
                    server_id: server_id.to_string(),
                    code_verifier,
                    client_id: client_id.clone(),
                    client_secret,
                    token_endpoint,
                    redirect_uri: redirect_uri.to_string(),
                    started_at: super::tokens::now_unix(),
                },
            );
        }

        let authorize_url = Url::parse_with_params(
            &authorization_endpoint,
            &[
                ("response_type", "code"),
                ("client_id", client_id.as_str()),
                ("redirect_uri", redirect_uri),
                ("code_challenge", code_challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("state", state.as_str()),
                ("resource", url.as_str()),
            ],
        )
        .map_err(|error| Error::malformed(format!("could not build an authorize url: {error}")))?;

        tracing::info!(server_id, client_id, "started an oauth authorization");
        Ok(authorize_url.to_string())
    }

    /// Finishes a sign-in from the redirect's `code` and `state`.
    ///
    /// Exchanges the code and stores the token. Returns the install the
    /// authorization was for, so the caller can reconnect it — see the module
    /// note on why reconnecting is not done here.
    ///
    /// The pending entry is consumed whether or not the exchange succeeds: a
    /// code is single-use, so a retry with the same state could never work and
    /// keeping the entry would only hold a secret in memory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when the state is unknown or
    /// expired, plus whatever the token endpoint returns.
    pub async fn complete(&self, store: &Store, state: &str, code: &str) -> Result<String> {
        let pending = {
            let mut pending = self.pending.lock();
            prune_expired(&mut pending);
            pending.remove(state)
        }
        .ok_or_else(|| Error::malformed("unknown or expired oauth state"))?;

        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", pending.redirect_uri.as_str()),
            ("client_id", pending.client_id.as_str()),
            ("code_verifier", pending.code_verifier.as_str()),
        ];
        if let Some(secret) = pending.client_secret.as_deref() {
            form.push(("client_secret", secret));
        }

        let body = post_form(&self.http, &pending.token_endpoint, &form).await?;
        let tokens = TokenResponse::parse(&body)?;

        persist(
            store,
            &pending.server_id,
            &pending.client_id,
            pending.client_secret.as_deref(),
            &pending.token_endpoint,
            &tokens,
        )?;

        tracing::info!(
            server_id = %pending.server_id,
            "completed an oauth authorization and stored the token"
        );
        Ok(pending.server_id)
    }

    /// How many authorizations are parked. For tests and diagnostics.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        let mut pending = self.pending.lock();
        prune_expired(&mut pending);
        pending.len()
    }

    /// Registers a client dynamically, per RFC 7591.
    ///
    /// A confidential client is requested, and the secret is kept when one is
    /// issued: some servers issue one regardless of what was asked for, and the
    /// token exchange then fails without it.
    async fn register_client(
        &self,
        registration_endpoint: &str,
        redirect_uri: &str,
    ) -> Result<(String, Option<String>)> {
        let body = json!({
            "client_name": CLIENT_NAME,
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "client_secret_post",
        });

        let response = self
            .http
            .post(registration_endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|error| Error::transport(registration_endpoint, error))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| Error::transport(registration_endpoint, error))?;

        if !status.is_success() {
            return Err(Error::Http {
                endpoint: crate::redact_endpoint(registration_endpoint),
                status: status.as_u16(),
                body: text,
            });
        }

        let registered: Value = serde_json::from_str(&text).map_err(|error| {
            Error::malformed(format!("client registration replied with non-json: {error}"))
        })?;

        let client_id = registered
            .get("client_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::malformed("client registration returned no client_id"))?
            .to_string();

        let client_secret = registered
            .get("client_secret")
            .and_then(Value::as_str)
            .map(ToString::to_string);

        Ok((client_id, client_secret))
    }

    /// The endpoint of an HTTP-remote install, or `None` for a subprocess one.
    fn remote_url(&self, store: &Store, server_id: &str) -> Result<Option<String>> {
        let server = store.get_server(server_id)?;
        Ok(match server.transport {
            Transport::HttpRemote { ref url } if !url.is_empty() => Some(url.clone()),
            _ => None,
        })
    }
}

/// Whether an authorization server will accept an authorization code.
///
/// An empty list counts as yes. RFC 8414 makes the field optional and says it
/// *defaults* to including `authorization_code`, and servers do omit it — so
/// requiring it would reject servers that work.
fn supports_authorization_code(metadata: &tinymcp_bus::AuthorizationServerMetadata) -> bool {
    metadata.grant_types_supported.is_empty()
        || metadata
            .grant_types_supported
            .iter()
            .any(|grant| grant == "authorization_code")
}

/// Generates a PKCE verifier and its S256 challenge.
///
/// The verifier is 48 bytes of entropy from three version-4 identifiers,
/// base64url-encoded to 64 characters — inside the 43-to-128 range the
/// specification requires.
fn generate_pkce() -> (String, String) {
    let mut bytes = Vec::with_capacity(48);
    for _ in 0..3 {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }

    let verifier = B64.encode(bytes);
    let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Drops authorizations the user never finished.
///
/// Each holds a client secret, so leaving them is a slow leak of credentials
/// into a map nothing ever reads again.
fn prune_expired(pending: &mut HashMap<String, PendingAuthorization>) {
    let cutoff = super::tokens::now_unix().saturating_sub(PENDING_TTL.as_secs());
    pending.retain(|_, authorization| authorization.started_at >= cutoff);
}

/// Exposes [`generate_pkce`] to the module's tests.
#[cfg(test)]
pub(super) fn generate_pkce_for_test() -> (String, String) {
    generate_pkce()
}
