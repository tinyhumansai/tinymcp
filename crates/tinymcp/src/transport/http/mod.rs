//! The Streamable HTTP transport.
//!
//! [`McpHttpClient`] speaks MCP over HTTP: the `initialize` handshake and
//! protocol-version negotiation, `tools/list` and `tools/call`, server-sent
//! event draining, session lifecycle through `Mcp-Session-Id`, OAuth discovery
//! from a `WWW-Authenticate` challenge, and a graceful `DELETE` on close.
//!
//! # Three behaviors worth knowing before you read the code
//!
//! **A 404 while holding a session means the session expired.** The client
//! reinitializes and retries the request exactly once. The retry is not itself
//! retried, so a server that answers 404 for some other reason costs one extra
//! round trip rather than an unbounded loop.
//!
//! **Redirects are followed, up to five.** Servers are commonly published
//! behind a vanity URL that redirects to the real endpoint. `reqwest` strips
//! `Authorization` and `Cookie` on a cross-origin redirect, so a bearer token
//! does not follow the request to another host.
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

mod headers;
mod sse;

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use parking_lot::Mutex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::transport::{redact_endpoint, render_tool_result, validate_protocol_version};
use headers::{
    apply_auth, header_to_string, mcp_param_headers_from_schema, parse_www_authenticate_challenge,
};
use sse::{first_complete_sse_data, parse_sse_events, parse_sse_message};
use tinymcp_bus::{
    AuthorizationServerMetadata, LATEST_PROTOCOL_VERSION, McpAuthConfig, McpAuthorizationContext,
    McpClientIdentityConfig, McpClientInfo, McpInitializeResult, McpProxyConfig, McpRemoteTool,
    McpServerToolResult, McpSseEvent, ProtectedResourceMetadata,
};

/// The `MCP-Protocol-Version` request header.
const HEADER_PROTOCOL_VERSION: &str = "MCP-Protocol-Version";
/// The `Mcp-Session-Id` header, carried in both directions.
const HEADER_SESSION_ID: &str = "Mcp-Session-Id";
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
    auth: McpAuthConfig,
    state: Mutex<SessionState>,
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
            // "MCP HTTP 301". `reqwest` strips `Authorization` and `Cookie` on
            // a cross-origin redirect, so a bearer token does not follow the
            // request to another host.
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS));

        if let Some(proxy) = self.proxy.as_ref() {
            builder = apply_proxy(builder, proxy);
        }

        let http = builder.build().map_err(|source| Error::ClientBuild {
            // Stripped for the reason on `Error::transport`: a proxy URL can
            // carry credentials, and this error is printed.
            source: Box::new(source.without_url()),
        })?;

        Ok(McpHttpClient {
            endpoint: self.endpoint,
            http,
            next_id: AtomicI64::new(1),
            client_info: McpClientInfo::from(&self.identity),
            auth: self.auth,
            state: Mutex::new(SessionState::default()),
        })
    }
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
    pub async fn initialize(&self) -> Result<McpInitializeResult> {
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
                "capabilities": {},
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
    pub async fn list_tools(&self) -> Result<Vec<McpRemoteTool>> {
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
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpServerToolResult> {
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

    /// Discovers how to authorize to this server, if it demands authorization.
    ///
    /// Sends an unauthenticated `initialize` and reads the `WWW-Authenticate`
    /// challenge from the 401. Returns `Ok(None)` when the server answers
    /// anything else, which is the "no authorization needed" case.
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
    pub async fn discover_authorization(&self) -> Result<Option<McpAuthorizationContext>> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.next_request_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": {},
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
    pub async fn drain_events(&self, last_event_id: Option<&str>) -> Result<Vec<McpSseEvent>> {
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
    /// Tries the `OpenID` Connect discovery document first and falls back to the
    /// OAuth authorization-server one. Servers publish one or the other and
    /// rarely say which.
    async fn fetch_authorization_server_metadata(
        &self,
        issuer: &str,
    ) -> Result<AuthorizationServerMetadata> {
        let trimmed = issuer.trim_end_matches('/');

        let oidc = format!("{trimmed}/.well-known/openid-configuration");
        if let Ok(metadata) = self.fetch_json::<AuthorizationServerMetadata>(&oidc).await {
            return Ok(metadata);
        }

        let oauth = format!("{trimmed}/.well-known/oauth-authorization-server");
        self.fetch_json::<AuthorizationServerMetadata>(&oauth).await
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
            // Typed rather than a string, so a caller decides on data. The
            // presence of `resource_metadata` is what separates a server that
            // wants OAuth from one that wants a static credential.
            return Err(Error::Unauthorized {
                endpoint: redact_endpoint(&self.endpoint),
                resource_metadata: parse_www_authenticate_challenge(&response_headers)
                    .and_then(|challenge| challenge.resource_metadata),
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

#[cfg(test)]
mod test;
