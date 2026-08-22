//! Configuration payload types.
//!
//! These describe the servers a host wants reachable, how to authenticate to
//! each of them, who the client says it is during the `initialize` handshake,
//! and how to reach the network. They are the argument to the module, not the
//! module's own settings file — nothing here is read from disk by the
//! implementation.

use serde::{Deserialize, Serialize};
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
}

impl Default for McpClientConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            servers: Vec::new(),
            client_identity: McpClientIdentityConfig::default(),
            registry_auth: McpRegistryAuthConfig::default(),
            proxy: None,
        }
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

impl McpRegistryAuthConfig {
    /// Returns a copy with every secret replaced by whether it was set.
    ///
    /// Use this on any path that reports configuration back to a caller. The
    /// non-secret [`Self::mcp_official_base`] is preserved, because a user who
    /// cannot see which registry they are pointed at cannot debug it.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp_bus::McpRegistryAuthConfig;
    /// let mut auth = McpRegistryAuthConfig::default();
    /// auth.smithery_api_key = Some("secret".into());
    ///
    /// let (redacted, smithery_set, _) = auth.redacted();
    /// assert!(smithery_set);
    /// assert_eq!(redacted.smithery_api_key, None);
    /// ```
    #[must_use]
    pub fn redacted(&self) -> (Self, bool, bool) {
        let smithery_set = self.smithery_api_key.is_some();
        let official_set = self.mcp_official_token.is_some();
        (
            Self {
                smithery_api_key: None,
                mcp_official_base: self.mcp_official_base.clone(),
                mcp_official_token: None,
            },
            smithery_set,
            official_set,
        )
    }
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
