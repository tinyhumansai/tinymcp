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
    McpInitializeResult, McpProxyConfig, McpRemoteTool, McpResource, McpResourceContents,
    McpServerConfig, McpServerToolResult,
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
    /// What decides which server this definition reaches; see
    /// [`Self::fingerprint`].
    fingerprint: String,
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

    /// A digest of everything that decides which server this definition
    /// reaches: endpoint, command, arguments, working directory, the names
    /// (never the values) of its environment, and its tool allow and deny
    /// lists. The tool cache is keyed on it, so an edited definition never
    /// reads the old one's tools.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The key this server's tools are cached under.
    #[must_use]
    pub fn cache_key(&self) -> String {
        crate::registry::store::static_cache_key(&self.name)
    }
}

/// Either transport, behind one interface.
///
/// Both arms are boxed. The two clients differ substantially in size — the HTTP
/// one carries a `reqwest` client and a session, the subprocess one a pair of
/// pipes — and an unboxed enum is as large as its biggest arm everywhere it is
/// stored, including in the registry's map of every configured server.
#[derive(Debug)]
#[non_exhaustive]
pub enum McpTransportClient {
    /// A Streamable HTTP server.
    Http(Box<McpHttpClient>),
    /// A subprocess server.
    Stdio(Box<McpStdioClient>),
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

    /// A tool from the transport's last listing, without a round trip.
    #[must_use]
    pub fn cached_tool(&self, tool: &str) -> Option<McpRemoteTool> {
        match self {
            Self::Http(client) => client.cached_tool(tool),
            Self::Stdio(client) => client.cached_tool(tool),
        }
    }

    /// Lists the resources the server advertises.
    ///
    /// # Errors
    ///
    /// Returns whatever the underlying transport returns.
    pub async fn list_resources(&self) -> Result<Vec<McpResource>> {
        match self {
            Self::Http(client) => client.list_resources().await,
            Self::Stdio(client) => client.list_resources().await,
        }
    }

    /// Reads one resource's contents.
    ///
    /// # Errors
    ///
    /// Returns whatever the underlying transport returns.
    pub async fn read_resource(&self, uri: &str) -> Result<Vec<McpResourceContents>> {
        match self {
            Self::Http(client) => client.read_resource(uri).await,
            Self::Stdio(client) => client.read_resource(uri).await,
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

    /// Lists a server's tools, as [`Self::list_tools`], and records the result
    /// in `store`'s tool cache so [`Self::cached_tools`] can answer without
    /// the network next time.
    ///
    /// # Errors
    ///
    /// As [`Self::list_tools`]. A failed cache write is logged, not returned:
    /// the listing the caller asked for succeeded.
    pub async fn list_tools_caching(
        &self,
        server: &str,
        store: &crate::registry::Store,
    ) -> Result<Vec<McpRemoteTool>> {
        let definition = self.require(server)?;
        let tools = self.list_tools(server).await?;
        let cached: Vec<tinymcp_bus::McpTool> = tools.iter().map(remote_to_cached).collect();
        if let Err(error) =
            store.put_cached_tools(&definition.cache_key(), definition.fingerprint(), &cached)
        {
            tracing::debug!(server = %definition.name, "could not cache the tool listing: {error}");
        }
        Ok(tools)
    }

    /// A server's tools from `store`'s tool cache, without dialling.
    ///
    /// `None` when nothing is cached for this server's current definition.
    /// The allow and deny lists are re-applied, so tightening them takes effect
    /// on cached tools at once.
    #[must_use]
    pub fn cached_tools(
        &self,
        server: &str,
        store: &crate::registry::Store,
    ) -> Option<Vec<tinymcp_bus::McpTool>> {
        let definition = self.get(server)?;
        match store.cached_tools(&definition.cache_key(), definition.fingerprint()) {
            Ok(cached) => cached.map(|cached| {
                cached
                    .tools
                    .into_iter()
                    .filter(|tool| definition.is_tool_allowed(&tool.name))
                    .collect()
            }),
            Err(error) => {
                tracing::debug!(server = %definition.name, "could not read the tool cache: {error}");
                None
            }
        }
    }

    /// Lists every server's tools and caches them, one at a time.
    ///
    /// For a host warming the cache in the background. Returns each server's
    /// name with the number of tools it advertised, or the error that stopped
    /// it; one unreachable server does not stop the rest.
    pub async fn refresh_tool_cache(
        &self,
        store: &crate::registry::Store,
    ) -> Vec<(String, Result<usize>)> {
        let mut outcomes = Vec::with_capacity(self.order.len());
        for name in &self.order {
            let outcome = self
                .list_tools_caching(name, store)
                .await
                .map(|tools| tools.len());
            outcomes.push((name.clone(), outcome));
        }
        outcomes
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

    /// A listed tool's `_meta`, from the transport's last listing.
    ///
    /// No round trip. `None` when the server or tool is unknown, the tool is
    /// not permitted, nothing has been listed yet, or the tool has no `_meta`.
    #[must_use]
    pub fn tool_meta(&self, server: &str, tool: &str) -> Option<Value> {
        let definition = self.get(server)?;
        if !definition.is_tool_allowed(tool) {
            return None;
        }
        definition.client.cached_tool(tool)?.meta
    }

    /// Lists a server's resources.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when `server` is not registered, plus
    /// whatever the transport returns.
    pub async fn list_resources(&self, server: &str) -> Result<Vec<McpResource>> {
        self.require(server)?.client.list_resources().await
    }

    /// Reads one of a server's resources.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when `server` is not registered,
    /// [`Error::ResourceTooLarge`] when the contents exceed
    /// [`tinymcp_bus::MAX_RESOURCE_BYTES`], plus whatever the transport
    /// returns.
    pub async fn read_resource(&self, server: &str, uri: &str) -> Result<Vec<McpResourceContents>> {
        self.require(server)?.client.read_resource(uri).await
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

    /// Ends a server's session.
    ///
    /// Worth calling rather than leaving to a drop: an HTTP server holds a
    /// session it will keep alive until it times out, and a host shutting down
    /// cleanly should not leave one behind on every server it declared.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when `server` is not registered, plus
    /// whatever the transport returns.
    pub async fn close_session(&self, server: &str) -> Result<()> {
        self.require(server)?.client.close_session().await
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
            fingerprint: static_fingerprint(server),
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
        return Ok(McpTransportClient::Http(Box::new(client)));
    }

    let env = server
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    Ok(McpTransportClient::Stdio(Box::new(McpStdioClient::new(
        command,
        server.args.clone(),
        env,
        server.cwd.as_ref().map(PathBuf::from),
        identity,
    ))))
}

/// Trims tool names, drops empties, and removes duplicates.
///
/// Order is preserved so a caller reading the list back sees what they wrote.
/// The fingerprint of a configured server; see
/// [`McpServerDefinition::fingerprint`].
fn static_fingerprint(server: &McpServerConfig) -> String {
    let mut env_keys: Vec<&str> = server.env.keys().map(String::as_str).collect();
    env_keys.sort_unstable();
    let mut env: Vec<(&str, &str)> = server
        .env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    env.sort_unstable_by_key(|(key, _)| *key);
    let env_fingerprint: Vec<String> = env
        .into_iter()
        .flat_map(|(key, value)| [key.to_string(), value.to_string()])
        .collect();
    let auth_parts = auth_fingerprint_parts(&server.auth);
    let mut allowed = normalize_tool_names(&server.allowed_tools);
    allowed.sort();
    let mut disallowed = normalize_tool_names(&server.disallowed_tools);
    disallowed.sort();
    let mut parts = vec![
        server.endpoint.trim().to_string(),
        server.command.trim().to_string(),
        server.args.join("\0"),
        server.cwd.clone().unwrap_or_default(),
        env_keys.join("\0"),
        auth_parts[0].clone(),
        allowed.join("\0"),
        disallowed.join("\0"),
    ];
    parts.extend(env_fingerprint);
    parts.extend(auth_parts.into_iter().skip(1));
    let references: Vec<&str> = parts.iter().map(String::as_str).collect();
    crate::registry::store::fingerprint(&references)
}

/// Stable, non-secret identity for the configured authentication scheme.
fn auth_fingerprint_identity(auth: &McpAuthConfig) -> String {
    match auth {
        McpAuthConfig::None => "none".to_string(),
        McpAuthConfig::BearerToken { .. } => "bearer".to_string(),
        McpAuthConfig::Basic { .. } => "basic".to_string(),
        McpAuthConfig::Header { name, .. } => format!("header:{name}"),
        McpAuthConfig::Headers { headers } => {
            let mut names: Vec<&str> = headers.iter().map(|header| header.name.as_str()).collect();
            names.sort_unstable();
            format!("headers:{}", names.join("\0"))
        }
        McpAuthConfig::QueryParam { name, .. } => format!("query:{name}"),
        _ => "other".to_string(),
    }
}

/// Inputs for the static cache digest; secret values are never stored directly.
fn auth_fingerprint_parts(auth: &McpAuthConfig) -> Vec<String> {
    let mut parts = vec![auth_fingerprint_identity(auth)];
    match auth {
        McpAuthConfig::BearerToken { token } => parts.push(token.clone()),
        McpAuthConfig::Basic { username, password } => {
            parts.push(username.clone());
            parts.push(password.clone());
        }
        McpAuthConfig::Header { value, .. } | McpAuthConfig::QueryParam { value, .. } => {
            parts.push(value.clone());
        }
        McpAuthConfig::Headers { headers } => {
            let mut values: Vec<(&str, &str)> = headers
                .iter()
                .map(|header| (header.name.as_str(), header.value.as_str()))
                .collect();
            values.sort_unstable();
            for (name, value) in values {
                parts.push(name.to_string());
                parts.push(value.to_string());
            }
        }
        McpAuthConfig::None | _ => {}
    }
    parts
}

/// A listed tool in the shape the cache stores.
fn remote_to_cached(tool: &McpRemoteTool) -> tinymcp_bus::McpTool {
    tinymcp_bus::McpTool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
    }
}

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
