//! The connection map and its lifecycle.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::RwLock;

use super::dial::{build_http_auth, credential_safe_dial_url, resolve_final_url};
use super::status::{ConnectFailure, classify};
use crate::error::{Error, Result};
use crate::registry::Store;
use crate::registry::oauth::{OAuthFlow, refresh_if_expired};
use crate::transport::http::McpHttpClient;
use crate::transport::stdio::McpStdioClient;
use tinymcp_bus::{
    ConnStatus, ConnectedServerOverview, InstalledServer, McpAuthConfig, McpClientIdentityConfig,
    McpProxyConfig, McpServerToolResult, McpTool, Transport,
};

/// The per-request timeout for a connected HTTP-remote server.
///
/// Matched to the timeout the setup flow's connection test uses, so a server
/// that passes the test behaves the same once installed.
const REMOTE_TIMEOUT_SECS: u64 = 30;

/// A live transport for one connected install.
#[derive(Debug)]
enum ActiveClient {
    /// A subprocess server.
    Stdio(Box<McpStdioClient>),
    /// An HTTP-remote server.
    Http(Box<McpHttpClient>),
}

impl ActiveClient {
    /// Lists the tools the server advertises.
    async fn list_tools(&self) -> Result<Vec<tinymcp_bus::McpRemoteTool>> {
        match self {
            Self::Stdio(client) => client.list_tools().await,
            Self::Http(client) => client.list_tools().await,
        }
    }

    /// Calls a tool.
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpServerToolResult> {
        match self {
            Self::Stdio(client) => client.call_tool(name, arguments).await,
            Self::Http(client) => client.call_tool(name, arguments).await,
        }
    }

    /// Ends the session.
    async fn close_session(&self) -> Result<()> {
        match self {
            Self::Stdio(client) => client.close_session().await,
            Self::Http(client) => client.close_session().await,
        }
    }
}

/// One connected server: its transport, its tools, and enough identity to
/// describe it without going back to the store.
#[derive(Debug)]
struct Connection {
    client: ActiveClient,
    tools: RwLock<Vec<McpTool>>,
    qualified_name: String,
    display_name: String,
    description: Option<String>,
}

impl Connection {
    /// A copy of the cached tool list.
    async fn tools_snapshot(&self) -> Vec<McpTool> {
        self.tools.read().await.clone()
    }
}

/// Everything currently connected, and why anything is not.
///
/// One instance per host. See the module documentation for why this is not a
/// global.
#[derive(Debug, Default)]
pub struct Connections {
    live: RwLock<HashMap<String, Arc<Connection>>>,
    failures: RwLock<HashMap<String, ConnectFailure>>,
}

impl Connections {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Brings a server up and caches what it advertises.
    ///
    /// Both transports finish with a handshake *and* a tool listing, so a
    /// misconfigured server fails here rather than silently at a user's first
    /// tool call.
    ///
    /// A success clears any recorded failure. A failure records one, classified
    /// so a caller knows whether to offer a sign-in, a token field, or an error.
    ///
    /// # Errors
    ///
    /// Returns whatever the transport returns. The failure is recorded either
    /// way, so a caller polling status sees it without re-attempting.
    pub async fn connect(
        &self,
        store: &Store,
        oauth: &OAuthFlow,
        identity: &McpClientIdentityConfig,
        proxy: Option<&McpProxyConfig>,
        server: &InstalledServer,
    ) -> Result<Vec<McpTool>> {
        let result = self
            .connect_inner(store, oauth, identity, proxy, server)
            .await;

        match &result {
            Ok(_) => {
                self.failures.write().await.remove(&server.server_id);
            }
            Err(error) => {
                // Whether a credential was sent is re-derived the same way the
                // dial derived it, so the reason has one source of truth. This
                // is the cold path; one extra store read costs nothing.
                let has_credential = store
                    .load_env_values(&server.server_id)
                    .is_ok_and(|env| !matches!(build_http_auth(&env), McpAuthConfig::None));

                let failure = ConnectFailure::new(error, has_credential);
                tracing::debug!(
                    server_id = %server.server_id,
                    auth = ?failure.auth,
                    "recorded a connection failure"
                );
                self.failures
                    .write()
                    .await
                    .insert(server.server_id.clone(), failure);
            }
        }

        result
    }

    /// The body of [`Self::connect`], without the failure bookkeeping.
    async fn connect_inner(
        &self,
        store: &Store,
        oauth: &OAuthFlow,
        identity: &McpClientIdentityConfig,
        proxy: Option<&McpProxyConfig>,
        server: &InstalledServer,
    ) -> Result<Vec<McpTool>> {
        tracing::debug!(
            server_id = %server.server_id,
            transport = server.transport.dispatch_kind(),
            "connecting"
        );

        let client = match &server.transport {
            Transport::Stdio => self.dial_stdio(store, identity, server).await?,
            Transport::HttpRemote { url } => {
                self.dial_remote(store, oauth, identity, proxy, server, url)
                    .await?
            }
            // The contract's transport enum is `#[non_exhaustive]`, so a
            // transport added by a newer contract compiles here rather than
            // breaking the build. Refusing to dial it is the only honest
            // option: this build has no code that speaks it, and guessing at
            // one of the two it does know would connect the user to the wrong
            // thing.
            other => {
                return Err(Error::malformed(format!(
                    "install `{}` uses the `{}` transport, which this build does not speak",
                    server.server_id,
                    other.dispatch_kind()
                )));
            }
        };

        let tools: Vec<McpTool> = client
            .list_tools()
            .await?
            .into_iter()
            .map(|remote| McpTool {
                name: remote.name,
                description: remote.description,
                input_schema: remote.input_schema,
            })
            .collect();

        let connection = Arc::new(Connection {
            client,
            tools: RwLock::new(tools.clone()),
            qualified_name: server.qualified_name.clone(),
            display_name: server.display_name.clone(),
            description: server.description.clone(),
        });

        self.live
            .write()
            .await
            .insert(server.server_id.clone(), connection);

        // Best effort: a connection that worked should not be reported as
        // failed because a timestamp could not be written.
        if let Err(error) = store.touch_last_connected(&server.server_id) {
            tracing::debug!(
                server_id = %server.server_id,
                "could not record the connection time: {error}"
            );
        }

        tracing::debug!(
            server_id = %server.server_id,
            tools = tools.len(),
            "connected"
        );
        Ok(tools)
    }

    /// Spawns and handshakes a subprocess server.
    async fn dial_stdio(
        &self,
        store: &Store,
        identity: &McpClientIdentityConfig,
        server: &InstalledServer,
    ) -> Result<ActiveClient> {
        let env: Vec<(String, String)> = store
            .load_env_values(&server.server_id)
            .unwrap_or_default()
            .into_iter()
            // Internal bookkeeping is not the child's business either.
            .filter(|(name, _)| !super::dial::is_internal_key(name))
            .collect();

        let client = McpStdioClient::new(
            server.command.clone(),
            server.args.clone(),
            env,
            None,
            identity,
        );
        client.initialize().await?;

        Ok(ActiveClient::Stdio(Box::new(client)))
    }

    /// Dials and handshakes an HTTP-remote server.
    async fn dial_remote(
        &self,
        store: &Store,
        oauth: &OAuthFlow,
        identity: &McpClientIdentityConfig,
        proxy: Option<&McpProxyConfig>,
        server: &InstalledServer,
        url: &str,
    ) -> Result<ActiveClient> {
        if url.is_empty() {
            return Err(Error::malformed(format!(
                "the http-remote install `{}` has no endpoint",
                server.server_id
            )));
        }

        // Refresh before dialling so a session never opens with a stale token.
        // A refresh that fails is not fatal: the existing token may still work,
        // and if it does not, the 401 tells the user to sign in again.
        if let Err(error) = refresh_if_expired(store, oauth.http(), &server.server_id).await {
            tracing::warn!(
                server_id = %server.server_id,
                "could not refresh the access token; dialling with the existing one: {error}"
            );
        }

        // Read credentials *after* the refresh, so a freshly minted token is
        // the one that gets sent.
        let auth = build_http_auth(&store.load_env_values(&server.server_id).unwrap_or_default());

        let resolved = resolve_final_url(url)
            .await
            .unwrap_or_else(|| url.to_string());
        let dial_url = credential_safe_dial_url(url, resolved);
        if dial_url != url {
            tracing::info!(
                from = %crate::redact_endpoint(url),
                to = %crate::redact_endpoint(&dial_url),
                "resolved a redirecting endpoint for the authenticated dial"
            );
        }

        let client = McpHttpClient::builder(dial_url)
            .timeout_secs(REMOTE_TIMEOUT_SECS)
            .auth(auth)
            .identity(identity.clone())
            .proxy(proxy.cloned())
            .build()?;
        client.initialize().await?;

        Ok(ActiveClient::Http(Box::new(client)))
    }

    /// Whether there is an entry for this server.
    ///
    /// Membership only. A transport that dropped silently stays in the map
    /// until something probes it — use [`Self::probe_alive`] for that.
    pub async fn is_connected(&self, server_id: &str) -> bool {
        self.live.read().await.contains_key(server_id)
    }

    /// Whether a connected server still answers, within `timeout`.
    ///
    /// Issues a real round trip. This is how a dead transport becomes visible
    /// before a user's next tool call finds it.
    ///
    /// A missing entry, a transport error, and a timeout all report `false`:
    /// each means "not usable, reconnect", and distinguishing them would give a
    /// caller a choice it has no different response to.
    pub async fn probe_alive(&self, server_id: &str, timeout: Duration) -> bool {
        let Some(connection) = self.get(server_id).await else {
            return false;
        };

        match tokio::time::timeout(timeout, connection.client.list_tools()).await {
            Ok(Ok(_)) => true,
            Ok(Err(error)) => {
                tracing::debug!(server_id, "probe found a broken transport: {error}");
                false
            }
            Err(_) => {
                tracing::debug!(server_id, ?timeout, "probe timed out");
                false
            }
        }
    }

    /// Ends a connection and forgets any recorded failure.
    ///
    /// Returns whether there was anything to disconnect.
    pub async fn disconnect(&self, server_id: &str) -> bool {
        let connection = self.live.write().await.remove(server_id);
        self.failures.write().await.remove(server_id);

        match connection {
            Some(connection) => {
                if let Err(error) = connection.client.close_session().await {
                    tracing::debug!(server_id, "closing the session failed: {error}");
                }
                true
            }
            None => false,
        }
    }

    /// Ends every connection.
    ///
    /// For a host shutting down. Recorded failures are left alone: they describe
    /// what happened, and shutting down does not change that.
    pub async fn disconnect_all(&self) {
        let connections: Vec<Arc<Connection>> =
            self.live.write().await.drain().map(|(_, c)| c).collect();

        for connection in connections {
            if let Err(error) = connection.client.close_session().await {
                tracing::debug!("closing a session during shutdown failed: {error}");
            }
        }
    }

    /// Calls a tool on a connected server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] when the server has no live connection,
    /// plus whatever the transport returns.
    pub async fn call_tool(
        &self,
        server_id: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<McpServerToolResult> {
        let connection = self
            .get(server_id)
            .await
            .ok_or_else(|| Error::NotConnected {
                server: server_id.to_string(),
            })?;

        connection.client.call_tool(tool, arguments).await
    }

    /// A status summary for every installed server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the installs cannot be listed.
    pub async fn all_status(&self, store: &Store) -> Result<Vec<ConnStatus>> {
        let installed = store.list_servers()?;

        // One snapshot of each map, so a server's status is decided from a
        // consistent view rather than from two reads that could disagree.
        let live: HashMap<String, Arc<Connection>> = self.live.read().await.clone();
        let failures = self.failures.read().await.clone();

        let mut statuses = Vec::with_capacity(installed.len());
        for server in installed {
            let connected_tool_count = match live.get(&server.server_id) {
                Some(connection) => {
                    Some(u32::try_from(connection.tools_snapshot().await.len()).unwrap_or(u32::MAX))
                }
                None => None,
            };

            let (status, tool_count, last_error, auth_hint) = classify(
                server.enabled,
                connected_tool_count,
                failures.get(&server.server_id),
            );

            statuses.push(ConnStatus {
                server_id: server.server_id,
                qualified_name: server.qualified_name,
                display_name: server.display_name,
                status,
                tool_count,
                last_error,
                auth_hint,
            });
        }

        Ok(statuses)
    }

    /// The tools of one connected server, or `None` when it is not connected.
    ///
    /// `Some(vec![])` means connected and advertising nothing. This is the cheap
    /// way to learn a server's tools without forcing a reconnect.
    pub async fn tools_for(&self, server_id: &str) -> Option<Vec<McpTool>> {
        Some(self.get(server_id).await?.tools_snapshot().await)
    }

    /// Every connected server's identity and tools.
    ///
    /// Sorted by qualified name. A caller rendering this into a prompt would
    /// otherwise see it reshuffle between turns purely from map iteration order,
    /// which costs a cached prefix for no reason.
    pub async fn connected_overview(&self) -> Vec<ConnectedServerOverview> {
        let snapshot: Vec<(String, Arc<Connection>)> = self
            .live
            .read()
            .await
            .iter()
            .map(|(id, connection)| (id.clone(), Arc::clone(connection)))
            .collect();

        let mut overviews = Vec::with_capacity(snapshot.len());
        for (server_id, connection) in snapshot {
            overviews.push(ConnectedServerOverview {
                server_id,
                qualified_name: connection.qualified_name.clone(),
                display_name: connection.display_name.clone(),
                description: connection.description.clone(),
                tools: connection.tools_snapshot().await,
            });
        }

        overviews.sort_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
        overviews
    }

    /// Every tool on every connected server, paired with its server.
    ///
    /// For a caller assembling one flat tool surface across servers.
    pub async fn all_connected_tools(&self) -> Vec<(String, String, McpTool)> {
        self.connected_overview()
            .await
            .into_iter()
            .flat_map(|overview| {
                let server_id = overview.server_id;
                let qualified_name = overview.qualified_name;
                overview
                    .tools
                    .into_iter()
                    .map(move |tool| (server_id.clone(), qualified_name.clone(), tool))
            })
            .collect()
    }

    /// The most recent failure message for a server, if it has one.
    pub async fn last_error(&self, server_id: &str) -> Option<String> {
        self.failures
            .read()
            .await
            .get(server_id)
            .map(|failure| failure.message.clone())
    }

    /// Why a server's most recent attempt hit a 401, if it did.
    pub async fn auth_hint(&self, server_id: &str) -> Option<tinymcp_bus::McpAuthHint> {
        self.failures
            .read()
            .await
            .get(server_id)
            .and_then(|failure| failure.auth)
    }

    /// Whether a server's most recent attempt failed for want of credentials.
    pub async fn needs_auth(&self, server_id: &str) -> bool {
        self.auth_hint(server_id).await.is_some()
    }

    /// Forgets a recorded failure.
    ///
    /// Called when the reason for it may have changed — a successful connect, a
    /// disconnect, an uninstall, a server being turned off.
    pub async fn clear_last_error(&self, server_id: &str) {
        self.failures.write().await.remove(server_id);
    }

    /// How many servers are connected.
    pub async fn connected_count(&self) -> usize {
        self.live.read().await.len()
    }

    /// One connection, cloned out from under the lock.
    async fn get(&self, server_id: &str) -> Option<Arc<Connection>> {
        self.live.read().await.get(server_id).cloned()
    }
}
