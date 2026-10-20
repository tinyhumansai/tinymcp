//! Configuration payload types.
//!
//! These describe the servers a host wants reachable, how to authenticate to
//! each of them, who the client says it is during the `initialize` handshake,
//! and how to reach the network. They are the argument to the module, not the
//! module's own settings file — nothing here is read from disk by the
//! implementation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The serde default for a flag that is on unless the user turns it off.
///
/// Present so every `enabled` field below can spell its default the same way
/// and a reader never has to check whether one of them is the odd one out.
const fn default_true() -> bool {
    true
}

/// The serde default for a per-request timeout, in seconds.
const fn default_timeout_secs() -> u64 {
    30
}

/// The complete MCP client configuration a host hands to the module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct McpClientConfig {
    /// When `true`, the statically declared servers below are exposed.
    ///
    /// This does not gate the dynamic registry; a user's installed servers are
    /// governed by their own per-server enabled flag.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The statically declared server set.
    ///
    /// These are the servers a host pins in its own configuration, as opposed
    /// to the ones a user installs at runtime through the registry.
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
    /// Who this client claims to be during the `initialize` handshake.
    #[serde(default)]
    pub client_identity: McpClientIdentityConfig,
    /// Credentials and endpoint overrides for the registry browse APIs.
    #[serde(default)]
    pub registry_auth: McpRegistryAuthConfig,
    /// The proxy to route outbound HTTP through, already resolved by the host.
    ///
    /// `None` means connect directly.
    #[serde(default)]
    pub proxy: Option<McpProxyConfig>,
    /// The jail every stdio server runs in, unless it is operator-trusted.
    ///
    /// `None` means no sandbox is available: a stdio server that is not
    /// [`McpServerConfig::operator_trusted`] (and every registry-installed
    /// one) is refused rather than started unconfined.
    #[serde(default)]
    pub stdio_sandbox: Option<StdioSandboxConfig>,
}

/// The jail a host grants stdio MCP servers.
///
/// The module starts each server through the `TinyBox` bus contract with these
/// grants, requiring the backend to enforce them. A backend that cannot is a
/// refusal, never a weaker launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct StdioSandboxConfig {
    /// Existing absolute directory: the server's writable root and the jail's
    /// workspace. A server's working directory must lie inside it.
    pub workspace: String,
    /// Existing absolute paths the server may read and execute, such as its
    /// runtime (`/usr`, a Node install).
    #[serde(default)]
    pub read_only: Vec<String>,
    /// Existing absolute paths the server may also write.
    #[serde(default)]
    pub read_write: Vec<String>,
    /// Absolute paths the server may neither read nor write (credential
    /// stores).
    #[serde(default)]
    pub deny_read: Vec<String>,
    /// Whether the server may use the network. MCP servers commonly call
    /// APIs, so this is on by default.
    #[serde(default = "default_true")]
    pub allow_net: bool,
    /// Whether the server may start child processes (`npx` does).
    #[serde(default = "default_true")]
    pub allow_subprocess: bool,
    /// Require the backend to meet the untrusted-code isolation floor. On by
    /// default; a host turns it off only for a backend it has judged enough.
    #[serde(default = "default_true")]
    pub require_process_isolation: bool,
    /// How strictly the backend must enforce the grants. The default,
    /// [`StdioSandboxEnforcement::Enforced`], refuses a backend that can only
    /// enforce part of them; a host that has judged a partial backend enough
    /// (for example macOS Seatbelt, which confines folders) opts in with
    /// [`StdioSandboxEnforcement::Declared`].
    #[serde(default)]
    pub enforcement: StdioSandboxEnforcement,
}

/// Required treatment of the jail's declared constraints. Mirrors `TinyBox`'s
/// `JailEnforcement` (this crate cannot depend on `tinybox-bus`), and
/// serializes the same way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StdioSandboxEnforcement {
    /// Require every requested filesystem, network and subprocess constraint;
    /// a backend that cannot enforce them refuses the start.
    #[default]
    Enforced,
    /// Accept the backend's advertised partial constraints. A host chooses
    /// this only for a backend it has approved.
    Declared,
}

impl Default for McpClientConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            servers: Vec::new(),
            client_identity: McpClientIdentityConfig::default(),
            registry_auth: McpRegistryAuthConfig::default(),
            proxy: None,
            stdio_sandbox: None,
        }
    }
}

/// A host opening a data directory with its own MCP client configuration.
///
/// The data directory is the storage identity; the client configuration is
/// the static server set, credentials, identity and proxy for that host. Both
/// cross the bus together so two workspaces opened by one module instance do
/// not inherit one another's settings. Hosts should send this payload through
/// `TinyBus`'s confidential-call path because `client` may contain credentials.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OpenConfiguredRequest {
    /// Absolute directory that owns this object's persistent state.
    pub data_dir: String,
    /// The settings this object uses for static and registry server access.
    pub client: McpClientConfig,
}

/// The object identity and cleanup transition created by `OpenConfigured`.
///
/// Replacing an object is queued behind in-flight `TinyBus` calls. Hosts use the
/// transition id with `TransitionStatus` to learn when the old service and its
/// transports have been shut down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OpenConfiguredResponse {
    /// Object path that will serve this directory after the transition.
    pub object_path: String,
    /// Opaque id for observing replacement and old-service cleanup.
    pub transition_id: String,
}

/// A nonblocking observation of a directory lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TransitionStatus {
    /// The object-tree update or owned-resource cleanup is still running.
    Pending,
    /// The object-tree update and all module-owned cleanup have completed.
    Complete,
    /// The transition failed; the prior owner remains available for retry.
    Failed,
    /// The token is unknown or its bounded observation record was retired.
    Unknown,
}

impl std::fmt::Debug for OpenConfiguredRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenConfiguredRequest")
            .finish_non_exhaustive()
    }
}

/// One statically declared MCP server.
///
/// Transport is chosen by what is filled in: a non-empty [`Self::command`]
/// means stdio, and otherwise the server is dialled over HTTP at
/// [`Self::endpoint`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct McpServerConfig {
    /// Stable slug identifying this server to callers.
    #[serde(default)]
    pub name: String,
    /// The Streamable HTTP endpoint URL. Ignored when [`Self::command`] is set.
    #[serde(default)]
    pub endpoint: String,
    /// The command to spawn for a stdio server. Non-empty selects stdio.
    #[serde(default)]
    pub command: String,
    /// Arguments for [`Self::command`].
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the spawned child. MCP stdio servers conventionally
    /// take their credentials this way.
    ///
    /// Ordered so the serialized form is stable: an unordered map would make
    /// the wire representation depend on hash iteration order, which is exactly
    /// the kind of thing a round-trip test cannot pin.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory for the spawned child.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Human-readable description surfaced to callers.
    #[serde(default)]
    pub description: Option<String>,
    /// Whether this server is exposed at all.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Exact tool names this server may expose.
    ///
    /// Empty means every tool is allowed unless it appears in
    /// [`Self::disallowed_tools`].
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Exact tool names that are always hidden and blocked.
    ///
    /// This list wins over [`Self::allowed_tools`].
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    /// The operator vouches for this stdio server, so it starts as an ordinary
    /// child process without the jail. Off by default; only the host's own
    /// configuration should set it, never a catalog or a model.
    #[serde(default)]
    pub operator_trusted: bool,
    /// Per-request timeout, in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// How to authenticate outbound requests to this server.
    ///
    /// This covers pre-provisioned credentials. Interactive OAuth is handled by
    /// the transport when a server answers with a challenge.
    #[serde(default)]
    pub auth: McpAuthConfig,
}

/// A safe summary of one statically configured server for host tool output.
///
/// The endpoint has user information, query parameters, and fragments removed;
/// authentication values are represented only by their kind and whether one
/// is configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct StaticServerSummary {
    /// Stable server name.
    pub name: String,
    /// Endpoint without user information, query, or fragment.
    pub endpoint: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Per-request timeout in seconds.
    pub timeout_secs: u64,
    /// Tools this server permits, or empty when unrestricted.
    pub allowed_tools: Vec<String>,
    /// Tools this server always denies.
    pub disallowed_tools: Vec<String>,
    /// Whether authentication is configured.
    pub auth_configured: bool,
    /// Authentication kind, never the credential value.
    pub auth_kind: String,
    /// Whether the entry came from host or user configuration.
    pub source: String,
}

/// One configured MCP tool call that does not enter static server discovery.
///
/// Hosts use this for a fixed product integration that should share the
/// module-owned transport without appearing in the user's configured server
/// list. Send it through `TinyBus` confidential delivery because `server` may
/// contain authentication values.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ConfiguredToolCallRequest {
    /// The server's transport and authentication configuration.
    pub server: McpServerConfig,
    /// The remote tool to call.
    pub tool: String,
    /// The tool's JSON arguments.
    pub arguments: Value,
}

impl std::fmt::Debug for ConfiguredToolCallRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfiguredToolCallRequest")
            .field("tool", &self.tool)
            .finish_non_exhaustive()
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            endpoint: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            description: None,
            enabled: default_true(),
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            operator_trusted: false,
            timeout_secs: default_timeout_secs(),
            auth: McpAuthConfig::None,
        }
    }
}

/// One HTTP header, for the multi-header [`McpAuthConfig::Headers`] variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct HttpHeader {
    /// The header name.
    pub name: String,
    /// The header value.
    pub value: String,
}

impl HttpHeader {
    /// Builds a header from a name and a value.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp_bus::HttpHeader;
    /// let header = HttpHeader::new("X-Client-Key", "abc");
    /// assert_eq!(header.name, "X-Client-Key");
    /// ```
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// How to authenticate outbound requests to a server.
///
/// The wire form is internally tagged on `kind`, so adding a variant is a minor
/// contract bump rather than a breaking one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum McpAuthConfig {
    /// Send no credentials.
    #[default]
    None,
    /// Send `Authorization: Bearer <token>`.
    BearerToken {
        /// The bearer token.
        token: String,
    },
    /// Send HTTP basic authentication.
    Basic {
        /// The username.
        username: String,
        /// The password.
        password: String,
    },
    /// Send one arbitrary request header.
    Header {
        /// The header name.
        name: String,
        /// The header value.
        value: String,
    },
    /// Send several request headers, all of them.
    ///
    /// For remotes that authenticate with more than one header — an API key
    /// plus an organization id, say. A single-header remote uses
    /// [`Self::Header`].
    Headers {
        /// The headers to apply.
        headers: Vec<HttpHeader>,
    },
    /// Append a query parameter to the request URL.
    QueryParam {
        /// The parameter name.
        name: String,
        /// The parameter value.
        value: String,
    },
}

/// Who the client claims to be during the `initialize` handshake.
///
/// A remote server sees these values and may log or display them, so a host
/// that wants to be identifiable sets them rather than taking the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct McpClientIdentityConfig {
    /// Sent as `initialize.clientInfo.name`.
    #[serde(default = "default_client_name")]
    pub name: String,
    /// Sent as `initialize.clientInfo.title`.
    #[serde(default = "default_client_title")]
    pub title: String,
    /// Sent as `initialize.clientInfo.version`.
    #[serde(default = "default_client_version")]
    pub version: String,
    /// Sent as `initialize.capabilities`, on every transport.
    ///
    /// An empty object by default. A host that supports a protocol extension
    /// declares it here, for example
    /// `{"extensions": {"io.modelcontextprotocol/ui": {"mimeTypes": ["text/html;profile=mcp-app"]}}}`.
    #[serde(
        default = "default_client_capabilities",
        skip_serializing_if = "is_empty_object"
    )]
    pub capabilities: Value,
}

fn default_client_capabilities() -> Value {
    Value::Object(serde_json::Map::new())
}

fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
}

fn default_client_name() -> String {
    "tinymcp".into()
}

fn default_client_title() -> String {
    "TinyMCP Client".into()
}

/// The default client version: this contract crate's package version.
///
/// A host that wants a remote server to see *its* version — which is usually
/// what a host wants — sets [`McpClientIdentityConfig::version`] explicitly.
/// The default identifies the client library, because that is the only thing
/// this crate can honestly speak for.
fn default_client_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

impl Default for McpClientIdentityConfig {
    fn default() -> Self {
        Self {
            name: default_client_name(),
            title: default_client_title(),
            version: default_client_version(),
            capabilities: default_client_capabilities(),
        }
    }
}

/// Credentials and endpoint overrides for the registry browse APIs.
///
/// Each field is optional so a host can supply it from user settings instead of
/// an environment variable; the implementation falls back to the documented
/// environment variable when a field is unset, which is what keeps existing
/// container deployments working unchanged.
///
/// # Secrets
///
/// The secret-bearing fields are **write-only over the bus**. A getter reports
/// whether each secret is set, never its value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct McpRegistryAuthConfig {
    /// Smithery API key. Falls back to `SMITHERY_API_KEY`.
    #[serde(default)]
    pub smithery_api_key: Option<String>,
    /// Base URL override for the official registry. Falls back to
    /// `MCP_OFFICIAL_REGISTRY_BASE`. Not a secret.
    #[serde(default)]
    pub mcp_official_base: Option<String>,
    /// Bearer token for the official registry. Falls back to
    /// `MCP_OFFICIAL_REGISTRY_TOKEN`.
    #[serde(default)]
    pub mcp_official_token: Option<String>,
}

/// A proxy for outbound HTTP, already resolved by the host.
///
/// # Why the host resolves it
///
/// Whether a proxy applies to a given service is host policy: a host typically
/// has a scope setting, a per-service allow list, and a `no_proxy` list, and it
/// consults all three before deciding. Sending the *decision* rather than the
/// *policy* keeps that logic in the one place that owns it, and means this
/// contract does not have to grow a copy of a proxy-scoping model that would
/// then have to stay in step with the host's.
///
/// So a host that has decided MCP traffic should not be proxied sends `None`,
/// not a populated value with the scope turned off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct McpProxyConfig {
    /// Proxy URL for `http://` requests.
    #[serde(default)]
    pub http_proxy: Option<String>,
    /// Proxy URL for `https://` requests.
    #[serde(default)]
    pub https_proxy: Option<String>,
    /// Proxy URL for every scheme, applied in addition to the two above.
    #[serde(default)]
    pub all_proxy: Option<String>,
    /// Hosts that must be reached directly, in `NO_PROXY` syntax.
    #[serde(default)]
    pub no_proxy: Vec<String>,
}
