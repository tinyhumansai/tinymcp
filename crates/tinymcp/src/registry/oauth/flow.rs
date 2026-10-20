//! Running the authorization-code flow.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::credentials::OAuthCredentialStore;
use super::endpoint_guard::{guard_endpoint, guarded_client};
use super::pending::{MemoryPendingStore, PendingAuthorization, PendingAuthorizationStore};
use super::tokens::{due_refresh, exchange_refresh, oauth_failure_reason, persist, post_form};
use super::types::{AuthDetection, TokenResponse};
use crate::error::{Error, Result};
use crate::transport::http::McpHttpClient;
use tinymcp_bus::McpProxyConfig;

/// URL-safe base64 without padding, which is what OAuth uses throughout.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// How long to wait on an authorization server.
const AUTH_SERVER_TIMEOUT_SECS: u64 = 20;

/// How long to wait when probing a server for what it wants.
const PROBE_TIMEOUT_SECS: u64 = 20;

/// The client name presented during dynamic registration when the host sets
/// none with [`OAuthFlow::with_client_name`].
///
/// A user sees it on the consent screen, so it names the library rather than
/// leaving it blank.
pub const DEFAULT_CLIENT_NAME: &str = "TinyMCP";

/// Runs browser sign-in for HTTP-remote servers.
///
/// Parks authorizations between the redirect out and back in a
/// [`PendingAuthorizationStore`]: this process's memory by default, so one
/// instance per host. Instances sharing a store with
/// [`Self::with_pending_store`] may each complete what another began.
#[derive(Debug)]
pub struct OAuthFlow {
    http: reqwest::Client,
    no_redirect_http: reqwest::Client,
    proxy: Option<McpProxyConfig>,
    pending: Arc<dyn PendingAuthorizationStore>,
    public_endpoints_only: bool,
    client_name: String,
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
        let no_redirect_http = reqwest::Client::builder()
            .timeout(Duration::from_secs(AUTH_SERVER_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|source| Error::ClientBuild {
                source: Box::new(source.without_url()),
            })?;

        Ok(Self {
            http,
            no_redirect_http,
            proxy,
            pending: Arc::new(MemoryPendingStore::new()),
            public_endpoints_only: false,
            client_name: DEFAULT_CLIENT_NAME.to_string(),
        })
    }

    /// Sets the client name sent during dynamic registration, which the
    /// authorization server shows the user on its consent screen.
    ///
    /// A blank name keeps [`DEFAULT_CLIENT_NAME`]. Surrounding whitespace is
    /// trimmed. Clients already registered keep the name they were registered
    /// with.
    #[must_use]
    pub fn with_client_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let name = name.trim();
        if !name.is_empty() {
            self.client_name = name.to_string();
        }
        self
    }

    /// Parks authorizations in `store` instead of this process's memory.
    ///
    /// For a host running several instances behind one callback: with a store
    /// they all share, the instance the redirect lands on completes a sign-in
    /// another began. See [`PendingAuthorizationStore`].
    #[must_use]
    pub fn with_pending_store(mut self, store: Arc<dyn PendingAuthorizationStore>) -> Self {
        self.pending = store;
        self
    }

    /// The client name sent during dynamic registration.
    #[must_use]
    pub fn client_name(&self) -> &str {
        &self.client_name
    }

    /// Refuses authorization, registration and token endpoints that are not
    /// `https` or that resolve to an internal address.
    ///
    /// Those endpoints are advertised by the server being signed in to and
    /// `POST`ed to from the host, so a host connecting to servers it does not
    /// trust turns this on. Off by default: a desktop host signs in to loopback
    /// development servers. The token endpoint is re-checked when a code is
    /// exchanged, so a host re-pointed between `begin` and `complete` is
    /// refused too, and again by [`Self::refresh`] before each refresh. The
    /// free [`refresh_if_expired`](super::refresh_if_expired) does not
    /// re-check; a host that turns this on refreshes through [`Self::refresh`].
    #[must_use]
    pub fn require_public_endpoints(mut self) -> Self {
        self.public_endpoints_only = true;
        self
    }

    /// The HTTP client this flow uses, for the refresh path.
    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        if self.public_endpoints_only {
            &self.no_redirect_http
        } else {
            &self.http
        }
    }

    /// Classifies what a server wants before it will talk.
    ///
    /// Decided by probing, not by reading registry metadata, which is often
    /// wrong about this. A server that answers without a challenge is open; one
    /// whose authorization server [`Self::begin`] can drive (authorize, token
    /// and dynamic-registration endpoints, and the authorization-code grant)
    /// wants a browser sign-in; anything else wants a static token. The
    /// authorization server is found from the challenge's `resource_metadata`,
    /// or, for a Bearer challenge without one, from the well-known metadata on
    /// the server's origin.
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
    pub async fn detect<S>(&self, store: &S, server_id: &str) -> Result<AuthDetection>
    where
        S: OAuthCredentialStore + ?Sized,
    {
        let Some(url) = store.remote_url(server_id).await? else {
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
                .find(|metadata| can_drive_sign_in(metadata))
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
    /// Returns [`Error::AuthDiscovery`] when the server advertises no
    /// authorization server, or none offers everything the flow needs, [`Error::MalformedResponse`] when
    /// registration answers with something unusable, plus whatever the
    /// transport returns.
    pub async fn begin<S>(&self, store: &S, server_id: &str, redirect_uri: &str) -> Result<String>
    where
        S: OAuthCredentialStore + ?Sized,
    {
        let url = store
            .remote_url(server_id)
            .await?
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
            .find(|metadata| can_drive_sign_in(metadata))
            .ok_or_else(|| Error::AuthDiscovery {
                detail: if context.authorization_server_metadata.is_empty() {
                    "the server advertises no authorization server".to_string()
                } else {
                    "no authorization server offers an authorize endpoint, a token endpoint, and \
                     dynamic client registration together"
                        .to_string()
                },
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

        let registration_http = self
            .registration_client(
                &authorization_endpoint,
                &token_endpoint,
                &registration_endpoint,
            )
            .await?;
        let (client_id, client_secret) = self
            .register_client(
                registration_http.as_ref().unwrap_or(&self.http),
                &registration_endpoint,
                redirect_uri,
            )
            .await?;

        let (code_verifier, code_challenge) = generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();

        self.pending
            .park(
                &state,
                PendingAuthorization {
                    server_id: server_id.to_string(),
                    code_verifier,
                    client_id: client_id.clone(),
                    client_secret,
                    token_endpoint,
                    redirect_uri: redirect_uri.to_string(),
                    started_at: super::tokens::now_unix(),
                },
            )
            .await?;

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
    /// keeping the entry would only hold a secret in the store. Taking it is
    /// atomic in the store, so two completions of one state exchange the code
    /// at most once.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when the state is unknown or
    /// expired, whatever the pending store reports when it cannot be read,
    /// plus whatever the token endpoint returns.
    pub async fn complete<S>(&self, store: &S, state: &str, code: &str) -> Result<String>
    where
        S: OAuthCredentialStore + ?Sized,
    {
        let now = super::tokens::now_unix();
        let pending = self
            .pending
            .take(state)
            .await?
            .filter(|pending| !pending.expired_at(now))
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

        let guarded_http = self.token_client(&pending.token_endpoint).await?;
        let http = guarded_http.as_ref().unwrap_or(&self.http);
        let body = post_form(http, &pending.token_endpoint, &form).await?;
        let tokens = TokenResponse::parse(&body)?;
        persist(
            store,
            &pending.server_id,
            &pending.client_id,
            pending.client_secret.as_deref(),
            &pending.token_endpoint,
            &tokens,
        )
        .await?;

        tracing::info!(
            server_id = %pending.server_id,
            "completed an oauth authorization and stored the token"
        );
        Ok(pending.server_id)
    }

    /// Mints a new access token when the stored one has expired, or is about
    /// to, as [`refresh_if_expired`](super::refresh_if_expired) does.
    ///
    /// Returns whether a refresh happened. Under
    /// [`Self::require_public_endpoints`] the bundle's token endpoint is
    /// checked again, and the refresh posted over a client pinned to the
    /// addresses it was checked against, so a bundle stored by an older build
    /// or edited in a host's store cannot aim a refresh at an internal address.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when the stored bundle cannot be
    /// read, when the token endpoint is refused by the guard, or when it answers
    /// with something unusable; [`Error::Http`] when it answers with a failure
    /// status; and [`Error::Transport`] when it cannot be reached.
    pub async fn refresh<S>(&self, store: &S, server_id: &str) -> Result<bool>
    where
        S: OAuthCredentialStore + ?Sized,
    {
        let Some(bundle) = due_refresh(store, server_id).await? else {
            return Ok(false);
        };
        let guarded_http = self.token_client(&bundle.token_endpoint).await?;
        let http = guarded_http.as_ref().unwrap_or(&self.http);
        exchange_refresh(store, http, server_id, &bundle).await
    }

    /// The server a parked authorization is for, without consuming it.
    ///
    /// A browser redirect carries no session, so a host keeping credentials
    /// per tenant learns which store to [`complete`](Self::complete) into from
    /// the returned `state` alone. `None` when the state is unknown or expired.
    ///
    /// # Errors
    ///
    /// Whatever the pending store reports when it cannot be read.
    pub async fn pending_server(&self, state: &str) -> Result<Option<String>> {
        let now = super::tokens::now_unix();
        Ok(self
            .pending
            .peek(state)
            .await?
            .filter(|parked| !parked.expired_at(now))
            .map(|parked| parked.server_id))
    }

    /// How many authorizations are parked. For tests and diagnostics.
    ///
    /// # Errors
    ///
    /// Whatever the pending store reports when it cannot be read.
    pub async fn pending_count(&self) -> Result<usize> {
        self.pending.count().await
    }

    pub(super) async fn registration_client(
        &self,
        authorization_endpoint: &str,
        token_endpoint: &str,
        registration_endpoint: &str,
    ) -> Result<Option<reqwest::Client>> {
        if self.public_endpoints_only {
            guard_endpoint(authorization_endpoint, "authorization").await?;
            guard_endpoint(token_endpoint, "token").await?;
            Ok(Some(
                guarded_client(registration_endpoint, "registration").await?,
            ))
        } else {
            Ok(None)
        }
    }

    pub(super) async fn token_client(
        &self,
        token_endpoint: &str,
    ) -> Result<Option<reqwest::Client>> {
        if self.public_endpoints_only {
            Ok(Some(guarded_client(token_endpoint, "token").await?))
        } else {
            Ok(None)
        }
    }

    /// Registers a client dynamically, per RFC 7591.
    ///
    /// A confidential client is requested, and the secret is kept when one is
    /// issued: some servers issue one regardless of what was asked for, and the
    /// token exchange then fails without it.
    async fn register_client(
        &self,
        http: &reqwest::Client,
        registration_endpoint: &str,
        redirect_uri: &str,
    ) -> Result<(String, Option<String>)> {
        let body = json!({
            "client_name": self.client_name,
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "client_secret_post",
        });

        let response = http
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
                body: oauth_failure_reason(&text),
            });
        }

        let registered: Value = serde_json::from_str(&text).map_err(|error| {
            Error::malformed(format!(
                "client registration replied with non-json: {error}"
            ))
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
}

/// Whether this flow can run a sign-in against an authorization server: it
/// advertises an authorize endpoint, a token endpoint and dynamic client
/// registration, and accepts an authorization code.
///
/// [`OAuthFlow::detect`] and [`OAuthFlow::begin`] both ask this one question,
/// so detection never offers a sign-in that `begin` would then refuse — a
/// server with no registration endpoint (Slack's, for one) reads as wanting a
/// static token, not as a Sign in button that leads nowhere.
fn can_drive_sign_in(metadata: &tinymcp_bus::AuthorizationServerMetadata) -> bool {
    metadata.authorization_endpoint.is_some()
        && metadata.token_endpoint.is_some()
        && metadata.registration_endpoint.is_some()
        && supports_authorization_code(metadata)
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
/// Exposes [`generate_pkce`] to the module's tests.
#[cfg(test)]
pub(super) fn generate_pkce_for_test() -> (String, String) {
    generate_pkce()
}
