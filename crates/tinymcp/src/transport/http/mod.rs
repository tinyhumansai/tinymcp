//! The Streamable HTTP transport.
//!
//! [`McpHttpClient`] speaks MCP over HTTP: the `initialize` handshake and
//! protocol-version negotiation, `tools/list` and `tools/call`,
//! `resources/list` and `resources/read`, server-sent
//! event draining, session lifecycle through `Mcp-Session-Id`, OAuth discovery
//! from a `WWW-Authenticate` challenge or the server's well-known metadata, and
//! a graceful `DELETE` on close.
//!
//! # Three behaviors worth knowing before you read the code
//!
//! **A 404 while holding a session means the session expired.** The client
//! reinitializes and retries the request exactly once. The retry is not itself
//! retried, so a server that answers 404 for some other reason costs one extra
//! round trip rather than an unbounded loop.
//!
//! **Redirects are followed, up to five, but an HTTPS→HTTP downgrade is
//! refused.** Servers are commonly published behind a vanity URL that redirects
//! to the real endpoint. `reqwest` strips `Authorization` and `Cookie` on a
//! cross-origin redirect, so a bearer token does not follow the request to
//! another host — but a same-origin downgrade, and any custom header or
//! query-param credential on any hop, are not stripped, so the policy itself
//! refuses a hop that would move the request from HTTPS to plaintext.
//!
//! **The SSE body is read incrementally.** See the `sse` module for why that is
//! load-bearing rather than an optimization.
//!
//! # Session state is behind a synchronous mutex
//!
//! Session state — the negotiated version, the session id, the cached tool
//! list — is read on every request and written rarely, and none of those
//! touches await. A synchronous mutex is the right shape; an async one held
//! across a request would serialize the transport onto one in-flight call.

mod discovery;
mod headers;
mod sse;

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use reqwest::{RequestBuilder, Response, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::transport::resources::{
    MAX_RESOURCE_PAGES, list_params, parse_read_result, parse_resource_page,
};
use crate::transport::{redact_endpoint, render_tool_result, validate_protocol_version};
use discovery::{DISCOVERY_BUDGET, WellKnownOutcome};
use headers::{
    apply_auth, header_to_string, mcp_param_headers_from_schema, parse_www_authenticate_challenge,
};
use sse::{first_complete_sse_data, parse_sse_events, parse_sse_message};
use tinymcp_bus::{
    AuthorizationServerMetadata, HEADER_PROTOCOL_VERSION, HEADER_SESSION_ID,
    LATEST_PROTOCOL_VERSION, McpAuthConfig, McpAuthorizationContext, McpClientIdentityConfig,
    McpClientInfo, McpInitializeResult, McpProxyConfig, McpRemoteTool, McpResource,
    McpResourceContents, McpServerToolResult, McpSseEvent, ProtectedResourceMetadata,
};

/// The `Mcp-Method` request header, which some servers route on.
const HEADER_METHOD: &str = "Mcp-Method";
/// The `Mcp-Name` request header, carrying the tool name on a call.
const HEADER_NAME: &str = "Mcp-Name";
/// Both response encodings the transport accepts, always sent together.
const MCP_HTTP_ACCEPT: &str = "application/json, text/event-stream";
/// How long to wait for a connection, independent of the per-request timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How many redirects to follow before giving up.
const MAX_REDIRECTS: usize = 5;

/// An MCP client speaking Streamable HTTP to one endpoint.
///
/// One instance is one session. Construct it with [`Self::new`] or
/// [`Self::builder`], and drop it or call [`Self::close_session`] when done.
#[derive(Debug)]
pub struct McpHttpClient {
    endpoint: String,
    http: reqwest::Client,
    next_id: AtomicI64,
    client_info: McpClientInfo,
    capabilities: Value,
    auth: McpAuthConfig,
    state: Mutex<SessionState>,
    discovery_http: reqwest::Client,
    well_known: Mutex<Option<WellKnownOutcome>>,
}

/// Everything about the current session, guarded together.
#[derive(Debug)]
struct SessionState {
    initialized: bool,
    negotiated_protocol_version: String,
    session_id: Option<String>,
    initialize: Option<McpInitializeResult>,
    cached_tools: HashMap<String, McpRemoteTool>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            initialized: false,
            negotiated_protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
            session_id: None,
            initialize: None,
            cached_tools: HashMap::new(),
        }
    }
}

/// Assembles an [`McpHttpClient`].
///
/// Only the endpoint is required. Everything else has a default that works for
/// an unauthenticated server.
#[derive(Debug, Clone)]
pub struct McpHttpClientBuilder {
    endpoint: String,
    timeout: Duration,
    auth: McpAuthConfig,
    identity: McpClientIdentityConfig,
    proxy: Option<McpProxyConfig>,
}

impl McpHttpClientBuilder {
    /// Starts a builder for `endpoint`.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            timeout: Duration::from_secs(30),
            auth: McpAuthConfig::None,
            identity: McpClientIdentityConfig::default(),
            proxy: None,
        }
    }

    /// Sets the per-request timeout.
    #[must_use]
    pub fn timeout_secs(mut self, seconds: u64) -> Self {
        self.timeout = Duration::from_secs(seconds);
        self
    }

    /// Sets the credentials applied to outbound requests.
    #[must_use]
    pub fn auth(mut self, auth: McpAuthConfig) -> Self {
        self.auth = auth;
        self
    }

    /// Sets who the client claims to be during the handshake.
    #[must_use]
    pub fn identity(mut self, identity: McpClientIdentityConfig) -> Self {
        self.identity = identity;
        self
    }

    /// Sets the capabilities sent in `initialize`, replacing those the
    /// identity carries.
    #[must_use]
    pub fn capabilities(mut self, capabilities: Value) -> Self {
        self.identity.capabilities = capabilities;
        self
    }

    /// Routes outbound requests through a proxy the host already resolved.
    #[must_use]
    pub fn proxy(mut self, proxy: Option<McpProxyConfig>) -> Self {
        self.proxy = proxy;
        self
    }

    /// Builds the client.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when the underlying HTTP client cannot be
    /// constructed — in practice a malformed proxy URL or an unusable TLS
    /// configuration.
    pub fn build(self) -> Result<McpHttpClient> {
        let mut builder = reqwest::Client::builder()
            .timeout(self.timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            // Servers are commonly published behind a vanity URL that redirects
            // to the real endpoint; refusing to follow it surfaces as a bare
            // "MCP HTTP 301". A custom policy follows up to `MAX_REDIRECTS`
            // hops but refuses an HTTPS→HTTP downgrade, which would carry any
            // attached credential over plaintext. `reqwest` strips
            // `Authorization` and `Cookie` on a cross-origin redirect, so a
            // bearer token does not follow the request to another host, but
            // custom headers, query-param credentials and same-origin
            // downgrades are not stripped — the policy closes that gap.
            .redirect(redirect_policy());

        let mut discovery_builder = reqwest::Client::builder()
            .timeout(DISCOVERY_BUDGET)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());

        if let Some(proxy) = self.proxy.as_ref() {
            builder = apply_proxy(builder, proxy);
            discovery_builder = apply_proxy(discovery_builder, proxy);
        }

        // Stripped for the reason on `Error::transport`: a proxy URL can carry
        // credentials, and this error is printed.
        let build_error = |source: reqwest::Error| Error::ClientBuild {
            source: Box::new(source.without_url()),
        };
        let http = builder.build().map_err(build_error)?;
        let discovery_http = discovery_builder.build().map_err(build_error)?;

        Ok(McpHttpClient {
            endpoint: self.endpoint,
            http,
            next_id: AtomicI64::new(1),
            client_info: McpClientInfo::from(&self.identity),
            capabilities: self.identity.capabilities,
            auth: self.auth,
            state: Mutex::new(SessionState::default()),
            discovery_http,
            well_known: Mutex::new(None),
        })
    }
}

/// The redirect policy every HTTP client uses.
///
/// Follows vanity-URL redirects (servers are commonly published behind one)
/// but caps the chain at [`MAX_REDIRECTS`] and refuses an HTTPS→HTTP downgrade:
/// a redirect that moves the request to plaintext after it has been over TLS
/// would carry any attached credential in the clear. The same-origin case is
/// the gap `reqwest` leaves open — it strips `Authorization` and `Cookie` only
/// on a cross-origin hop, so a bearer on a same-host downgrade and any custom
/// header or query-param credential on any hop would otherwise follow. A
/// refused downgrade surfaces as a redirect error rather than a silent leak.
fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        // `previous[0]` is the initial URL (reqwest counts it, not a redirect),
        // so it is the scheme the host configured — and, for a credentialed
        // non-loopback endpoint, the one already required to be HTTPS.
        let origin_scheme = attempt.previous().first().map(reqwest::Url::scheme);
        match redirect_decision(
            origin_scheme,
            attempt.url().scheme(),
            attempt.previous().len(),
        ) {
            RedirectDecision::Follow => attempt.follow(),
            RedirectDecision::Error(msg) => attempt.error(msg),
        }
    })
}

/// What [`redirect_policy`] decides for one hop, as a pure function of its
/// inputs so the rule is unit-testable without standing up a redirect server.
///
/// `hops` is `previous.len()`, matching reqwest's `Limit` accounting: the
/// initial URL is counted, so a chain that has followed `MAX_REDIRECTS`
/// redirects reports `MAX_REDIRECTS + 1`.
#[derive(Debug, PartialEq, Eq)]
enum RedirectDecision {
    Follow,
    Error(&'static str),
}

fn redirect_decision(
    origin_scheme: Option<&str>,
    target_scheme: &str,
    hops: usize,
) -> RedirectDecision {
    if origin_scheme == Some("https") && target_scheme == "http" {
        return RedirectDecision::Error(
            "refusing an https→http redirect that would expose credentials in cleartext",
        );
    }
    if hops > MAX_REDIRECTS {
        return RedirectDecision::Error("too many redirects");
    }
    RedirectDecision::Follow
}

/// Applies a resolved proxy to a client builder.
///
/// An unusable proxy URL is logged and skipped rather than failing the build:
/// the alternative is a host that cannot reach *any* server because one of its
/// three proxy settings is malformed.
fn apply_proxy(
    mut builder: reqwest::ClientBuilder,
    proxy: &McpProxyConfig,
) -> reqwest::ClientBuilder {
    let no_proxy = if proxy.no_proxy.is_empty() {
        None
    } else {
        reqwest::NoProxy::from_string(&proxy.no_proxy.join(","))
    };

    let candidates: [(&str, Option<&String>); 3] = [
        ("all", proxy.all_proxy.as_ref()),
        ("http", proxy.http_proxy.as_ref()),
        ("https", proxy.https_proxy.as_ref()),
    ];

    for (kind, url) in candidates {
        let Some(url) = url else { continue };
        let built = match kind {
            "all" => reqwest::Proxy::all(url),
            "http" => reqwest::Proxy::http(url),
            _ => reqwest::Proxy::https(url),
        };
        match built {
            Ok(configured) => {
                builder = builder.proxy(configured.no_proxy(no_proxy.clone()));
            }
            Err(error) => {
                tracing::warn!(kind, "ignoring an unusable {kind}_proxy url: {error}");
            }
        }
    }

    builder
}

impl McpHttpClient {
    /// Builds a client for `endpoint` with a per-request timeout and no
    /// credentials.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when the HTTP client cannot be built.
    pub fn new(endpoint: impl Into<String>, timeout_secs: u64) -> Result<Self> {
        McpHttpClientBuilder::new(endpoint)
            .timeout_secs(timeout_secs)
            .build()
    }

    /// Starts a builder for `endpoint`.
    #[must_use]
    pub fn builder(endpoint: impl Into<String>) -> McpHttpClientBuilder {
        McpHttpClientBuilder::new(endpoint)
    }

    /// The endpoint this client dials, unredacted.
    ///
    /// Pass it through [`redact_endpoint`] before logging or displaying it.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The `initialize` result from this session, if the handshake has run.
    ///
    /// Does not perform the handshake; use [`Self::initialize`] for that.
    #[must_use]
    pub fn initialize_snapshot(&self) -> Option<McpInitializeResult> {
        self.state.lock().initialize.clone()
    }

    /// Performs the `initialize` handshake, or returns the cached result.
    ///
    /// On success the negotiated version and session id are recorded and a
    /// `notifications/initialized` is sent, which the protocol requires before
    /// any other request.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedProtocolVersion`] when the server settles on
    /// a version this client does not speak, [`Error::Unauthorized`] on a 401,
    /// and [`Error::Http`] or [`Error::Transport`] for other failures.
    ///
    /// Boxed at the method boundary so the async body is compiled once, inside
    /// this crate, rather than re-instantiated by every downstream crate or
    /// codegen unit that awaits it. Callers `.await` the returned future as
    /// before.
    #[inline(never)]
    pub fn initialize(&self) -> BoxFuture<'_, Result<McpInitializeResult>> {
        Box::pin(self.initialize_inner())
    }

    async fn initialize_inner(&self) -> Result<McpInitializeResult> {
        if let Some(existing) = self.state.lock().initialize.clone() {
            return Ok(existing);
        }

        let id = self.next_request_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": self.capabilities,
                "clientInfo": self.client_info,
            },
        });

        let request = self
            .apply_auth(self.post_json())
            .body(serde_json::to_vec(&body)?);
        let response = self.read_response(self.send(request).await?).await?;

        let initialized: McpInitializeResult = serde_json::from_value(response.result.clone())
            .map_err(|error| Error::malformed(format!("initialize result: {error}")))?;
        validate_protocol_version(&initialized.protocol_version)?;

        {
            let mut state = self.state.lock();
            state.initialized = true;
            state
                .negotiated_protocol_version
                .clone_from(&initialized.protocol_version);
            state.session_id.clone_from(&response.session_id);
            state.initialize = Some(initialized.clone());
        }

        self.send_notification("notifications/initialized", json!({}))
            .await?;

        Ok(initialized)
    }

    /// Lists the tools the server advertises, caching them for
    /// [`Self::call_tool`].
    ///
    /// Initializes the session first if it is not already up.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when the reply has no `tools`
    /// member, plus anything [`Self::initialize`] can return.
    ///
    /// Boxed at the method boundary so the async body is compiled once, inside
    /// this crate, rather than re-instantiated by every downstream crate or
    /// codegen unit that awaits it. Callers `.await` the returned future as
    /// before.
    #[inline(never)]
    pub fn list_tools(&self) -> BoxFuture<'_, Result<Vec<McpRemoteTool>>> {
        Box::pin(self.list_tools_inner())
    }

    async fn list_tools_inner(&self) -> Result<Vec<McpRemoteTool>> {
        self.initialize().await?;

        let result = self
            .send_jsonrpc(
                "tools/list",
                json!({}),
                RequestOptions::standard("tools/list", None, Vec::new()),
            )
            .await?
            .result;

        let tools = result
            .get("tools")
            .ok_or_else(|| Error::malformed("tools/list response has no `tools` member"))?;
        let tools: Vec<McpRemoteTool> = serde_json::from_value(tools.clone())
            .map_err(|error| Error::malformed(format!("tools/list entries: {error}")))?;

        self.state.lock().cached_tools = tools
            .iter()
            .cloned()
            .map(|tool| (tool.name.clone(), tool))
            .collect();

        Ok(tools)
    }

    /// Calls `name` with `arguments`.
    ///
    /// Looks the tool up — from the cache, or by listing if it is not cached —
    /// so any `x-mcp-header` properties in its schema can be mirrored into
    /// request headers.
    ///
    /// A tool that reports failure comes back as an [`McpServerToolResult`]
    /// whose `rendered` is flagged an error, not as an `Err`. The call
    /// succeeded; the tool said no.
    ///
    /// # Errors
    ///
    /// Returns whatever the transport returns, plus
    /// [`Error::MalformedResponse`] when a schema-tagged header cannot be
    /// encoded.
    ///
    /// Boxed at the method boundary so the async body is compiled once, inside
    /// this crate, rather than re-instantiated by every downstream crate or
    /// codegen unit that awaits it. Callers `.await` the returned future as
    /// before.
    #[inline(never)]
    pub fn call_tool<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> BoxFuture<'a, Result<McpServerToolResult>> {
        Box::pin(self.call_tool_inner(name, arguments))
    }

    async fn call_tool_inner(&self, name: &str, arguments: Value) -> Result<McpServerToolResult> {
        self.initialize().await?;

        let cached = self.state.lock().cached_tools.get(name).cloned();
        let tool = match cached {
            Some(tool) => Some(tool),
            None => self
                .list_tools()
                .await?
                .into_iter()
                .find(|tool| tool.name == name),
        };

        let extra_headers = match tool.as_ref() {
            Some(tool) => mcp_param_headers_from_schema(tool, &arguments)?,
            None => Vec::new(),
        };

        let result = self
            .send_jsonrpc(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                RequestOptions::standard("tools/call", Some(name), extra_headers),
            )
            .await?
            .result;

        let rendered = render_tool_result(&result);
        Ok(McpServerToolResult::new(result, rendered))
    }

    /// A tool from the last listing, without a round trip.
    ///
    /// `None` when the tool was not in it or nothing has been listed yet.
    #[must_use]
    pub fn cached_tool(&self, name: &str) -> Option<McpRemoteTool> {
        self.state.lock().cached_tools.get(name).cloned()
    }

    /// Lists the resources the server advertises, following pagination.
    ///
    /// Stops after a bounded number of pages, so a server that never stops
    /// handing out cursors cannot hold the caller forever.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when a page has no `resources`
    /// member, plus anything [`Self::initialize`] can return.
    pub async fn list_resources(&self) -> Result<Vec<McpResource>> {
        self.initialize().await?;

        let mut resources = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_RESOURCE_PAGES {
            let result = self
                .send_jsonrpc(
                    "resources/list",
                    list_params(cursor.as_deref()),
                    RequestOptions::standard("resources/list", None, Vec::new()),
                )
                .await?
                .result;
            let (page, next) = parse_resource_page(&result)?;
            resources.extend(page);
            match next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        tracing::debug!(
            endpoint = %redact_endpoint(&self.endpoint),
            resources = resources.len(),
            "listed resources"
        );
        Ok(resources)
    }

    /// Reads one resource's contents, verbatim.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ResourceTooLarge`] when the contents exceed
    /// [`tinymcp_bus::MAX_RESOURCE_BYTES`], [`Error::MalformedResponse`] when
    /// the reply has no `contents`, plus anything [`Self::initialize`] can
    /// return.
    pub async fn read_resource(&self, uri: &str) -> Result<Vec<McpResourceContents>> {
        self.initialize().await?;

        let result = self
            .send_jsonrpc(
                "resources/read",
                json!({ "uri": uri }),
                RequestOptions::standard("resources/read", None, Vec::new()),
            )
            .await?
            .result;
        let contents = parse_read_result(uri, &result)?;
        tracing::debug!(
            endpoint = %redact_endpoint(&self.endpoint),
            parts = contents.len(),
            "read a resource"
        );
        Ok(contents)
    }

    /// Discovers how to authorize to this server, if it demands authorization.
    ///
    /// Sends an unauthenticated `initialize` and reads the `WWW-Authenticate`
    /// challenge from the 401. Returns `Ok(None)` when the server answers
    /// anything else, which is the "no authorization needed" case.
    ///
    /// A challenge naming `resource_metadata` is followed. A Bearer challenge
    /// that names none is looked up on the server's origin instead: protected
    /// resource metadata at `/.well-known/oauth-protected-resource` under the
    /// endpoint's path and then at the root, and failing that the origin's own
    /// RFC 8414 or `OpenID` metadata when its issuer is the origin. Neither the
    /// context's shape nor its meaning changes: an empty authorization-server
    /// list still means none was found.
    ///
    /// An authorization server whose metadata cannot be fetched is omitted
    /// rather than failing the whole discovery: a protected resource may name
    /// several, and one being unreachable should not hide the others.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MissingAuthChallenge`] when the 401 carries no readable
    /// challenge, and [`Error::AuthDiscovery`] when the advertised
    /// protected-resource metadata cannot be fetched.
    ///
    /// Boxed at the method boundary so the async body is compiled once, inside
    /// this crate, rather than re-instantiated by every downstream crate or
    /// codegen unit that awaits it. Callers `.await` the returned future as
    /// before.
    #[inline(never)]
    pub fn discover_authorization(&self) -> BoxFuture<'_, Result<Option<McpAuthorizationContext>>> {
        Box::pin(self.discover_authorization_inner())
    }

    async fn discover_authorization_inner(&self) -> Result<Option<McpAuthorizationContext>> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.next_request_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": self.capabilities,
                "clientInfo": self.client_info,
            },
        });

        let request = self
            .apply_auth(self.post_json())
            .body(serde_json::to_vec(&body)?);
        let response = self.send(request).await?;

        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(None);
        }

        let challenge = parse_www_authenticate_challenge(response.headers())
            .ok_or(Error::MissingAuthChallenge)?;

        let protected_resource_metadata = match challenge.resource_metadata.as_deref() {
            Some(url) => Some(
                self.fetch_json::<ProtectedResourceMetadata>(url)
                    .await
                    .map_err(|error| Error::AuthDiscovery {
                        detail: format!("fetching protected-resource metadata: {error}"),
                        challenge: Box::new(challenge.clone()),
                    })?,
            ),
            None => None,
        };

        if protected_resource_metadata.is_none()
            && is_bearer(&challenge.scheme)
            && let WellKnownOutcome::Found(found) = self.well_known_authorization().await
        {
            return Ok(Some(McpAuthorizationContext {
                challenge,
                protected_resource_metadata: found.protected_resource_metadata,
                authorization_server_metadata: found.authorization_servers,
            }));
        }

        let mut authorization_server_metadata = Vec::new();
        if let Some(metadata) = protected_resource_metadata.as_ref() {
            for issuer in &metadata.authorization_servers {
                match self.fetch_authorization_server_metadata(issuer).await {
                    Ok(found) => authorization_server_metadata.push(found),
                    Err(error) => tracing::debug!(
                        issuer = %redact_endpoint(issuer),
                        "skipping an authorization server whose metadata could not be read: {error}"
                    ),
                }
            }
        }

        Ok(Some(McpAuthorizationContext {
            challenge,
            protected_resource_metadata,
            authorization_server_metadata,
        }))
    }

    /// Reads any pending server-sent events.
    ///
    /// `last_event_id` resumes a stream from where a previous read stopped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] when the stream endpoint answers with a failure
    /// status, plus anything [`Self::initialize`] can return.
    ///
    /// Boxed at the method boundary so the async body is compiled once, inside
    /// this crate, rather than re-instantiated by every downstream crate or
    /// codegen unit that awaits it. Callers `.await` the returned future as
    /// before.
    #[inline(never)]
    pub fn drain_events<'a>(
        &'a self,
        last_event_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<McpSseEvent>>> {
        Box::pin(self.drain_events_inner(last_event_id))
    }

    async fn drain_events_inner(&self, last_event_id: Option<&str>) -> Result<Vec<McpSseEvent>> {
        self.initialize().await?;

        let (protocol_version, session_id) = {
            let state = self.state.lock();
            (
                state.negotiated_protocol_version.clone(),
                state.session_id.clone(),
            )
        };

        let mut request = self
            .apply_auth(self.http.get(&self.endpoint))
            .header(ACCEPT, "text/event-stream")
            .header(HEADER_PROTOCOL_VERSION, protocol_version);
        if let Some(session_id) = session_id {
            request = request.header(HEADER_SESSION_ID, session_id);
        }
        if let Some(last_event_id) = last_event_id {
            request = request.header("Last-Event-ID", last_event_id);
        }

        let response = self.send(request).await?;
        let status = response.status();
        let text = self.read_text(response).await?;
        if !status.is_success() {
            return Err(self.http_error(status, text));
        }

        parse_sse_events(&text)
    }

    /// Ends the session with an HTTP `DELETE` and clears local state.
    ///
    /// A server that answers `405 Method Not Allowed` is treated as success: it
    /// simply does not implement session deletion, which the protocol permits.
    /// Returns immediately when there is no session to close.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] when the server answers with any other failure
    /// status.
    pub async fn close_session(&self) -> Result<()> {
        let session_id = self.state.lock().session_id.clone();
        let Some(session_id) = session_id else {
            return Ok(());
        };

        let response = self
            .send(
                self.http
                    .delete(&self.endpoint)
                    .header(HEADER_SESSION_ID, session_id),
            )
            .await?;

        let status = response.status();
        if !(status.is_success() || status == StatusCode::METHOD_NOT_ALLOWED) {
            let text = self.read_text(response).await.unwrap_or_default();
            return Err(self.http_error(status, text));
        }

        self.reset_session();
        Ok(())
    }

    // -- internals ----------------------------------------------------------

    /// The next JSON-RPC request id.
    fn next_request_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// A POST to the endpoint with the two content headers every request needs.
    fn post_json(&self) -> RequestBuilder {
        self.http
            .post(&self.endpoint)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, MCP_HTTP_ACCEPT)
    }

    /// Applies this client's configured credentials.
    fn apply_auth(&self, request: RequestBuilder) -> RequestBuilder {
        apply_auth(request, &self.auth)
    }

    /// Sends a request, mapping a transport failure to a redacted error.
    async fn send(&self, request: RequestBuilder) -> Result<Response> {
        request
            .send()
            .await
            .map_err(|error| Error::transport(&self.endpoint, error))
    }

    /// Reads a response body as text.
    async fn read_text(&self, response: Response) -> Result<String> {
        response
            .text()
            .await
            .map_err(|error| Error::transport(&self.endpoint, error))
    }

    /// Builds a redacted [`Error::Http`].
    fn http_error(&self, status: StatusCode, body: String) -> Error {
        Error::Http {
            endpoint: redact_endpoint(&self.endpoint),
            status: status.as_u16(),
            body,
        }
    }

    /// Sends a fire-and-forget notification, which carries no reply.
    async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let request = self
            .apply_standard_headers(self.post_json(), method, None, &[])
            .body(serde_json::to_vec(&body)?);

        let response = self.send(request).await?;
        let status = response.status();
        if !status.is_success() {
            let text = self.read_text(response).await.unwrap_or_default();
            return Err(self.http_error(status, text));
        }
        Ok(())
    }

    /// Sends a JSON-RPC request, retrying once if the session has expired.
    async fn send_jsonrpc(
        &self,
        method: &str,
        params: Value,
        options: RequestOptions,
    ) -> Result<ResponseEnvelope> {
        self.send_jsonrpc_inner(method, params, options, true).await
    }

    /// The body of [`Self::send_jsonrpc`].
    ///
    /// `allow_reinitialize` is what bounds the retry: the retry itself is
    /// dispatched with it `false`, so a server that answers 404 for a reason
    /// other than an expired session costs one extra round trip rather than
    /// looping.
    async fn send_jsonrpc_inner(
        &self,
        method: &str,
        params: Value,
        options: RequestOptions,
        allow_reinitialize: bool,
    ) -> Result<ResponseEnvelope> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.next_request_id(),
            "method": method,
            "params": params,
        });

        tracing::debug!(
            endpoint = %redact_endpoint(&self.endpoint),
            method,
            "dispatching an mcp request"
        );

        let request = self
            .apply_standard_headers(
                self.post_json(),
                options.method_header.unwrap_or(method),
                options.name_header.as_deref(),
                &options.extra_headers,
            )
            .body(serde_json::to_vec(&body)?);

        let response = self.send(request).await?;

        let session_expired = response.status() == StatusCode::NOT_FOUND
            && allow_reinitialize
            && self.state.lock().session_id.is_some();

        if session_expired {
            tracing::info!(
                endpoint = %redact_endpoint(&self.endpoint),
                method,
                "session expired with 404; reinitializing and retrying once"
            );
            self.reset_session();
            self.initialize().await?;
            return Box::pin(self.send_jsonrpc_inner(method, params, options, false)).await;
        }

        self.read_response(response).await
    }

    /// Adds credentials, the routing headers, and the session headers.
    fn apply_standard_headers(
        &self,
        request: RequestBuilder,
        method: &str,
        name: Option<&str>,
        extra_headers: &[(HeaderName, HeaderValue)],
    ) -> RequestBuilder {
        let (protocol_version, session_id) = {
            let state = self.state.lock();
            (
                state.negotiated_protocol_version.clone(),
                state.session_id.clone(),
            )
        };

        let mut request = self.apply_auth(request).header(HEADER_METHOD, method);
        if let Some(name) = name {
            request = request.header(HEADER_NAME, name);
        }
        request = request.header(HEADER_PROTOCOL_VERSION, protocol_version);
        if let Some(session_id) = session_id {
            request = request.header(HEADER_SESSION_ID, session_id);
        }
        for (name, value) in extra_headers {
            request = request.header(name, value);
        }
        request
    }

    /// Fetches and decodes a JSON document from an arbitrary URL.
    async fn fetch_json<T>(&self, url: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|error| Error::transport(url, error))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| Error::transport(url, error))?;

        if !status.is_success() {
            return Err(Error::Http {
                endpoint: redact_endpoint(url),
                status: status.as_u16(),
                body: text,
            });
        }

        serde_json::from_str(&text).map_err(|error| {
            Error::malformed(format!("json from {}: {error}", redact_endpoint(url)))
        })
    }

    /// Reads an authorization server's metadata.
    ///
    /// Prefers the OAuth authorization-server document (RFC 8414), as the MCP
    /// authorization spec does, and falls back to `OpenID` Connect discovery.
    /// A server that publishes both may leave fields out of one of them — for
    /// example an OIDC document without the `registration_endpoint` that
    /// dynamic client registration needs — so a complete RFC 8414 document
    /// wins outright, and an incomplete one has its gaps filled from OIDC.
    async fn fetch_authorization_server_metadata(
        &self,
        issuer: &str,
    ) -> Result<AuthorizationServerMetadata> {
        let issuer_url = issuer.trim_end_matches('/');
        let oauth_url = rfc8414_metadata_url(issuer)?;
        let oidc_url = format!("{issuer_url}/.well-known/openid-configuration");

        let oauth = self
            .fetch_json::<AuthorizationServerMetadata>(&oauth_url)
            .await;
        match oauth {
            Ok(metadata) => {
                if validate_metadata_issuer(&metadata, issuer).is_err() {
                    tracing::debug!(issuer = %redact_endpoint(issuer_url), "[mcp] rfc 8414 issuer did not match, trying oidc discovery");
                    let oidc = self
                        .fetch_json::<AuthorizationServerMetadata>(&oidc_url)
                        .await?;
                    validate_metadata_issuer(&oidc, issuer)?;
                    return Ok(oidc);
                }
                if has_every_endpoint(&metadata) {
                    return Ok(metadata);
                }
                match self
                    .fetch_json::<AuthorizationServerMetadata>(&oidc_url)
                    .await
                {
                    Ok(oidc) if metadata_matches_issuer(&oidc, issuer) => {
                        Ok(fill_missing_metadata(metadata, oidc))
                    }
                    Ok(_) => Ok(metadata),
                    Err(error) => {
                        tracing::debug!(
                            issuer = %redact_endpoint(issuer_url),
                            "[mcp] oidc discovery unavailable to complete rfc 8414 metadata: {error}"
                        );
                        Ok(metadata)
                    }
                }
            }
            Err(oauth_error) => {
                tracing::debug!(
                    issuer = %redact_endpoint(issuer_url),
                    "[mcp] rfc 8414 metadata unavailable, trying oidc discovery: {oauth_error}"
                );
                let metadata = self
                    .fetch_json::<AuthorizationServerMetadata>(&oidc_url)
                    .await?;
                validate_metadata_issuer(&metadata, issuer)?;
                Ok(metadata)
            }
        }
    }

    /// Clears every trace of the current session.
    fn reset_session(&self) {
        *self.state.lock() = SessionState::default();
    }

    /// Turns a response into a JSON-RPC result, or the right error.
    async fn read_response(&self, response: Response) -> Result<ResponseEnvelope> {
        let status = response.status();
        let response_headers = response.headers().clone();

        if status == StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized {
                endpoint: redact_endpoint(&self.endpoint),
                resource_metadata: self.oauth_metadata_url(&response_headers).await,
            });
        }

        if !status.is_success() {
            let text = self.read_text(response).await.unwrap_or_default();
            return Err(self.http_error(status, text));
        }

        let content_type = response_headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();

        let payload = if content_type.starts_with("text/event-stream") {
            self.read_sse_payload(response).await?
        } else {
            let text = self.read_text(response).await?;
            serde_json::from_str(&text).map_err(|error| {
                Error::malformed(format!("response body is not json: {error} — {text}"))
            })?
        };

        if let Some(error) = payload.get("error") {
            return Err(Error::Rpc {
                message: error.to_string(),
            });
        }

        let result = payload
            .get("result")
            .ok_or_else(|| Error::malformed(format!("response has no `result`: {payload}")))?
            .clone();

        Ok(ResponseEnvelope {
            result,
            session_id: header_to_string(&response_headers, HEADER_SESSION_ID),
        })
    }

    /// The metadata URL that shows a 401 wants OAuth, or `None` for a static
    /// credential.
    ///
    /// The challenge's own `resource_metadata` when it has one. Otherwise, for
    /// a Bearer challenge, the well-known document that yielded an
    /// authorization server with authorize and token endpoints.
    async fn oauth_metadata_url(&self, headers: &reqwest::header::HeaderMap) -> Option<String> {
        let challenge = parse_www_authenticate_challenge(headers)?;
        if challenge.resource_metadata.is_some() {
            return challenge.resource_metadata;
        }
        if !is_bearer(&challenge.scheme) {
            return None;
        }
        match self.well_known_authorization().await {
            WellKnownOutcome::Found(found) if found.offers_sign_in() => {
                tracing::debug!(
                    endpoint = %redact_endpoint(&self.endpoint),
                    "[mcp] a 401 without resource_metadata found oauth metadata on the origin"
                );
                Some(found.metadata_url)
            }
            _ => None,
        }
    }

    /// Reads an SSE body only as far as the first data frame.
    ///
    /// A server may hold the stream open after replying; stopping at the reply
    /// is what keeps a call from waiting out the request timeout. The timeout
    /// still bounds a server that never replies at all.
    async fn read_sse_payload(&self, response: Response) -> Result<Value> {
        let mut raw: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| Error::transport(&self.endpoint, error))?;
            raw.extend_from_slice(&chunk);
            // Decode the whole buffer each pass, so a multi-byte character
            // split across a chunk boundary is never seen as corrupt.
            if let Some(data) = first_complete_sse_data(&String::from_utf8_lossy(&raw))? {
                return Ok(data);
            }
        }

        // The stream ended without a terminated data frame. The whole-body
        // parser gives a clearer error, and recovers a final frame that was
        // never followed by a blank line.
        parse_sse_message(&String::from_utf8_lossy(&raw))
    }
}

fn is_bearer(scheme: &str) -> bool {
    scheme.eq_ignore_ascii_case("bearer")
}

fn rfc8414_metadata_url(issuer: &str) -> Result<String> {
    let mut url = Url::parse(issuer).map_err(|error| {
        Error::malformed(format!("invalid authorization server issuer: {error}"))
    })?;
    let path = if url.path() == "/" { "" } else { url.path() };
    url.set_path(&format!("/.well-known/oauth-authorization-server{path}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn metadata_matches_issuer(metadata: &AuthorizationServerMetadata, issuer: &str) -> bool {
    metadata.issuer == issuer
}

fn validate_metadata_issuer(metadata: &AuthorizationServerMetadata, issuer: &str) -> Result<()> {
    if metadata_matches_issuer(metadata, issuer) {
        Ok(())
    } else {
        Err(Error::malformed(
            "authorization server metadata issuer did not match",
        ))
    }
}

/// The per-request knobs that vary between JSON-RPC calls.
#[derive(Debug, Clone)]
struct RequestOptions {
    method_header: Option<&'static str>,
    name_header: Option<String>,
    extra_headers: Vec<(HeaderName, HeaderValue)>,
}

impl RequestOptions {
    /// Options for an ordinary post-handshake request.
    fn standard(
        method_header: &'static str,
        name_header: Option<&str>,
        extra_headers: Vec<(HeaderName, HeaderValue)>,
    ) -> Self {
        Self {
            method_header: Some(method_header),
            name_header: name_header.map(ToString::to_string),
            extra_headers,
        }
    }
}

/// A decoded JSON-RPC reply and the session id that came with it.
#[derive(Debug, Clone)]
struct ResponseEnvelope {
    result: Value,
    session_id: Option<String>,
}

/// Whether metadata names every endpoint the browser sign-in flow needs.
fn has_every_endpoint(metadata: &AuthorizationServerMetadata) -> bool {
    metadata.authorization_endpoint.is_some()
        && metadata.token_endpoint.is_some()
        && metadata.registration_endpoint.is_some()
}

/// Fills what `primary` leaves out from `secondary`, never overriding it.
///
/// Only when both documents describe the same issuer: metadata for another
/// issuer is not a trustworthy source of endpoints.
fn fill_missing_metadata(
    mut primary: AuthorizationServerMetadata,
    secondary: AuthorizationServerMetadata,
) -> AuthorizationServerMetadata {
    if primary.issuer != secondary.issuer {
        return primary;
    }

    primary.authorization_endpoint = primary
        .authorization_endpoint
        .or(secondary.authorization_endpoint);
    primary.token_endpoint = primary.token_endpoint.or(secondary.token_endpoint);
    primary.registration_endpoint = primary
        .registration_endpoint
        .or(secondary.registration_endpoint);
    if primary.response_types_supported.is_empty() {
        primary.response_types_supported = secondary.response_types_supported;
    }
    if primary.code_challenge_methods_supported.is_empty() {
        primary.code_challenge_methods_supported = secondary.code_challenge_methods_supported;
    }
    primary
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;

#[cfg(test)]
#[path = "mod_ui_tests.rs"]
mod ui_test;
