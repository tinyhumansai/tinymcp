//! The static registry, its server definitions, and the transport dispatch.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::transport::http::McpHttpClient;
use crate::transport::stdio::McpStdioClient;
use tinymcp_bus::{
    McpAuthConfig, McpAuthorizationContext, McpClientConfig, McpClientIdentityConfig,
    McpInitializeResult, McpProxyConfig, McpRemoteTool, McpServerConfig, McpServerToolResult,
};

/// Where a server in the static set came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum McpRegistrySource {
    /// Declared by the user in the configuration handed to the module.
    Config,
    /// Seeded by the host itself rather than by the user.
    ///
    /// A host may pin a server of its own — its documentation, say. Marking
    /// those distinctly lets a caller show the difference between "you added
    /// this" and "this came with the application", and lets a user's own entry
    /// of the same name take precedence.
    Host,
}

/// One server in the static set, with its transport already built.
#[derive(Debug, Clone)]
pub struct McpServerDefinition {
    /// The slug callers name this server by.
    pub name: String,
    /// The HTTP endpoint, empty for a stdio server.
    pub endpoint: String,
    /// The spawned command, `None` for an HTTP server.
    pub command: Option<String>,
    /// A human-readable description.
    pub description: Option<String>,
    /// Tools this server may expose. Empty means "any not denied".
    pub allowed_tools: Vec<String>,
    /// Tools that are always blocked. Wins over [`Self::allowed_tools`].
    pub disallowed_tools: Vec<String>,
    /// The per-request timeout, in seconds.
    pub timeout_secs: u64,
    /// How outbound requests to this server authenticate.
    pub auth: McpAuthConfig,
    /// Where this entry came from.
    pub source: McpRegistrySource,
    /// The transport, shared so the registry stays cheap to clone.
    client: Arc<McpTransportClient>,
}

impl McpServerDefinition {
    /// Whether `tool` may be called on this server.
    ///
    /// Fail-closed: an empty or whitespace-only name is rejected, the deny list
    /// is consulted first and wins, and a non-empty allow list excludes
    /// everything not on it.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{McpClientConfig, McpServerConfig, McpServerRegistry};
    /// let config = McpClientConfig {
    ///     servers: vec![McpServerConfig {
    ///         name: "weather".into(),
    ///         endpoint: "https://example.test/mcp".into(),
    ///         allowed_tools: vec!["forecast".into()],
    ///         disallowed_tools: vec!["forecast".into()],
    ///         ..McpServerConfig::default()
    ///     }],
    ///     ..McpClientConfig::default()
    /// };
    /// let registry = McpServerRegistry::from_config(&config)?;
    /// let server = registry.get("weather").expect("the server");
    ///
    /// // Listed on both lists: denied. The deny list wins.
    /// assert!(!server.is_tool_allowed("forecast"));
    /// assert!(!server.is_tool_allowed(""));
    /// # Ok::<(), tinymcp::Error>(())
    /// ```
    #[must_use]
    pub fn is_tool_allowed(&self, tool: &str) -> bool {
        let tool = tool.trim();
        if tool.is_empty() {
            return false;
        }
        if self.disallowed_tools.iter().any(|name| name == tool) {
            return false;
        }
        self.allowed_tools.is_empty() || self.allowed_tools.iter().any(|name| name == tool)
    }

    /// Keeps only the tools [`Self::is_tool_allowed`] permits.
    #[must_use]
    pub fn filter_allowed_tools(&self, tools: Vec<McpRemoteTool>) -> Vec<McpRemoteTool> {
        tools
            .into_iter()
            .filter(|tool| self.is_tool_allowed(&tool.name))
            .collect()
    }

    /// Whether this server is dialled as a subprocess.
    #[must_use]
    pub const fn is_stdio(&self) -> bool {
        self.command.is_some()
    }
}

/// Either transport, behind one interface.
#[derive(Debug)]
#[non_exhaustive]
pub enum McpTransportClient {
    /// A Streamable HTTP server.
    Http(McpHttpClient),
    /// A subprocess server.
    Stdio(McpStdioClient),
}

impl McpTransportClient {
    /// Performs the handshake.
    ///
    /// # Errors
    ///
    /// Returns whatever the underlying transport returns.
    pub async fn initialize(&self) -> Result<McpInitializeResult> {
        match self {
            Self::Http(client) => client.initialize().await,
            Self::Stdio(client) => client.initialize().await,
        }
    }

    /// Lists the tools the server advertises.
    ///
    /// # Errors
    ///
    /// Returns whatever the underlying transport returns.
    pub async fn list_tools(&self) -> Result<Vec<McpRemoteTool>> {
        match self {
            Self::Http(client) => client.list_tools().await,
            Self::Stdio(client) => client.list_tools().await,
        }
    }

    /// Calls a tool.
    ///
    /// # Errors
    ///
    /// Returns whatever the underlying transport returns.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<McpServerToolResult> {
        match self {
            Self::Http(client) => client.call_tool(tool, arguments).await,
            Self::Stdio(client) => client.call_tool(tool, arguments).await,
        }
    }

    /// Discovers how to authorize, when the transport has a notion of it.
    ///
    /// A subprocess server always reports `None`: there is no 401 and no
    /// challenge on a pipe, and a stdio server that needs a credential takes it
    /// through its environment.
    ///
    /// # Errors
    ///
    /// Returns whatever the underlying transport returns.
    pub async fn discover_authorization(&self) -> Result<Option<McpAuthorizationContext>> {
        match self {
            Self::Http(client) => client.discover_authorization().await,
            Self::Stdio(_) => Ok(None),
        }
    }

    /// Ends the session.
    ///
    /// # Errors
    ///
    /// Returns whatever the underlying transport returns.
    pub async fn close_session(&self) -> Result<()> {
        match self {
            Self::Http(client) => client.close_session().await,
            Self::Stdio(client) => client.close_session().await,
        }
    }
}

/// The static set, in the order it was declared.
///
/// Cheap to clone: the definitions share their transports, so a clone is a
/// second view of the same sessions rather than a second set of connections.
#[derive(Debug, Default, Clone)]
pub struct McpServerRegistry {
    by_name: BTreeMap<String, McpServerDefinition>,
    order: Vec<String>,
}

impl McpServerRegistry {
    /// Builds the registry from a host's configuration.
    ///
    /// Returns an empty registry when the configuration is disabled. An entry
    /// that is turned off, unnamed, or has neither an endpoint nor a command is
    /// skipped with a warning rather than failing the whole build — one
    /// malformed entry should not cost a user every other server they
    /// configured.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when an HTTP transport cannot be
    /// constructed, which in practice means a malformed proxy or an unusable
    /// TLS configuration — conditions that would affect every server, not one.
    pub fn from_config(config: &McpClientConfig) -> Result<Self> {
        let mut registry = Self::default();
        if !config.enabled {
            return Ok(registry);
        }

        for server in &config.servers {
            registry.register(
                server,
                &config.client_identity,
                config.proxy.as_ref(),
                McpRegistrySource::Config,
            )?;
        }

        Ok(registry)
    }

    /// Adds a server the host seeds itself, unless the user already declared
    /// one by that name.
    ///
    /// The user's entry wins. A host pinning its own documentation server
    /// should not override a user who deliberately pointed that name somewhere
    /// else.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when the transport cannot be built.
    pub fn seed_host_server(
        &mut self,
        server: &McpServerConfig,
        identity: &McpClientIdentityConfig,
        proxy: Option<&McpProxyConfig>,
    ) -> Result<()> {
        if self.get(server.name.trim()).is_some() {
            return Ok(());
        }
        self.register(server, identity, proxy, McpRegistrySource::Host)
    }

    /// Whether the registry holds no servers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// How many servers the registry holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Every server, in declaration order.
    #[must_use]
    pub fn list(&self) -> Vec<&McpServerDefinition> {
        self.order
            .iter()
            .filter_map(|name| self.by_name.get(name))
            .collect()
    }

    /// One server by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&McpServerDefinition> {
        self.by_name.get(name)
    }

    /// A copy holding only the servers named in `allowed`, case-insensitively.
    ///
    /// For scoping the surface to a caller's own allow list. An empty slice
    /// yields an empty registry — that is a caller who selected no servers, not
    /// a caller who selected all of them. A caller meaning "everything" should
    /// not call this at all.
    #[must_use]
    pub fn retaining_servers(&self, allowed: &[String]) -> Self {
        let allowed: HashSet<String> = allowed
            .iter()
            .map(|name| name.trim().to_ascii_lowercase())
            .collect();

        let mut filtered = Self::default();
        for name in &self.order {
            if allowed.contains(&name.to_ascii_lowercase())
                && let Some(definition) = self.by_name.get(name)
            {
                filtered.insert(definition.clone());
            }
        }

        tracing::debug!(
            before = self.order.len(),
            after = filtered.order.len(),
            "scoped the static registry to an allow list"
        );
        filtered
    }

    /// Lists a server's tools, filtered to what it is permitted to expose.
    ///
    /// The returned descriptions and titles are still remote text. Read them
    /// through the display accessors, and run any detector the host wants over
    /// them — see the module note on why that scanning is not done here.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when `server` is not registered, plus
    /// whatever the transport returns.
    pub async fn list_tools(&self, server: &str) -> Result<Vec<McpRemoteTool>> {
        let definition = self.require(server)?;
        let tools = definition.client.list_tools().await?;
        Ok(definition.filter_allowed_tools(tools))
    }

    /// Calls a tool on a server.
    ///
    /// The permission check runs *before* the transport, so a blocked call
    /// makes no request at all.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when `server` is not registered,
    /// [`Error::ToolNotAllowed`] when the tool is blocked, plus whatever the
    /// transport returns.
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<McpServerToolResult> {
        let definition = self.require(server)?;
        let tool = tool.trim();

        if !definition.is_tool_allowed(tool) {
            return Err(Error::ToolNotAllowed {
                server: definition.name.clone(),
                tool: tool.to_string(),
            });
        }

        definition.client.call_tool(tool, arguments).await
    }

    /// Performs a server's handshake.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when `server` is not registered, plus
    /// whatever the transport returns.
    pub async fn initialize(&self, server: &str) -> Result<McpInitializeResult> {
        self.require(server)?.client.initialize().await
    }

    /// Discovers how to authorize to a server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when `server` is not registered, plus
    /// whatever the transport returns.
    pub async fn discover_authorization(
        &self,
        server: &str,
    ) -> Result<Option<McpAuthorizationContext>> {
        self.require(server)?.client.discover_authorization().await
    }

    /// Looks a server up, or reports that it is not registered.
    fn require(&self, server: &str) -> Result<&McpServerDefinition> {
        self.get(server).ok_or_else(|| Error::UnknownServer {
            server: server.to_string(),
        })
    }

    /// Builds and inserts one configured server.
    fn register(
        &mut self,
        server: &McpServerConfig,
        identity: &McpClientIdentityConfig,
        proxy: Option<&McpProxyConfig>,
        source: McpRegistrySource,
    ) -> Result<()> {
        if !server.enabled {
            return Ok(());
        }

        let name = server.name.trim();
        let endpoint = server.endpoint.trim();
        let command = server.command.trim();

        if name.is_empty() || (endpoint.is_empty() && command.is_empty()) {
            tracing::warn!(
                name = %server.name,
                "skipping a malformed server entry: it has no name, or neither an endpoint nor a command"
            );
            return Ok(());
        }

        self.insert(McpServerDefinition {
            name: name.to_string(),
            endpoint: endpoint.to_string(),
            command: (!command.is_empty()).then(|| command.to_string()),
            description: server.description.clone(),
            allowed_tools: normalize_tool_names(&server.allowed_tools),
            disallowed_tools: normalize_tool_names(&server.disallowed_tools),
            timeout_secs: server.timeout_secs,
            auth: server.auth.clone(),
            source,
            client: Arc::new(build_transport(server, identity, proxy)?),
        });

        Ok(())
    }

    /// Inserts a definition, preserving first-declared order.
    fn insert(&mut self, definition: McpServerDefinition) {
        let name = definition.name.clone();
        if self.by_name.insert(name.clone(), definition).is_none() {
            self.order.push(name);
        }
    }
}

/// Chooses and builds the transport for one configured server.
///
/// A non-empty command selects the subprocess transport; otherwise the server
/// is dialled over HTTP.
fn build_transport(
    server: &McpServerConfig,
    identity: &McpClientIdentityConfig,
    proxy: Option<&McpProxyConfig>,
) -> Result<McpTransportClient> {
    let command = server.command.trim();

    if command.is_empty() {
        let client = McpHttpClient::builder(server.endpoint.trim())
            .timeout_secs(server.timeout_secs)
            .auth(server.auth.clone())
            .identity(identity.clone())
            .proxy(proxy.cloned())
            .build()?;
        return Ok(McpTransportClient::Http(client));
    }

    let env = server
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    Ok(McpTransportClient::Stdio(McpStdioClient::new(
        command,
        server.args.clone(),
        env,
        server.cwd.as_ref().map(PathBuf::from),
        identity,
    )))
}

/// Trims tool names, drops empties, and removes duplicates.
///
/// Order is preserved so a caller reading the list back sees what they wrote.
fn normalize_tool_names(tools: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::new();
    for tool in tools {
        let tool = tool.trim();
        if !tool.is_empty() && !normalized.iter().any(|existing| existing == tool) {
            normalized.push(tool.to_string());
        }
    }
    normalized
}
