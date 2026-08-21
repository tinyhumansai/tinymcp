//! Registry payload types: install records, connection status, and the
//! upstream registry DTOs.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Extra fields an upstream registry sent that this contract does not model.
///
/// Ordered so the serialized form does not depend on hash iteration order.
pub type ExtraFields = BTreeMap<String, Value>;

// ---------------------------------------------------------------------------
// CommandKind
// ---------------------------------------------------------------------------

/// How to launch an installed server's subprocess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum CommandKind {
    /// Launched through `npx`, from the Node ecosystem.
    Node,
    /// Launched through `uvx`, from the Python ecosystem.
    Python,
    /// A binary executed directly.
    Binary,
}

impl CommandKind {
    /// The stable string this kind is persisted and transmitted as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Python => "python",
            Self::Binary => "binary",
        }
    }

    /// Parses a persisted string, falling back to [`Self::Node`].
    ///
    /// The fallback is deliberate: `npx` is what the overwhelming majority of
    /// registry listings use, so an unrecognised value is far more likely to be
    /// a stale row than a new ecosystem.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp_bus::CommandKind;
    /// assert_eq!(CommandKind::parse("python"), CommandKind::Python);
    /// assert_eq!(CommandKind::parse("nonsense"), CommandKind::Node);
    /// ```
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "python" => Self::Python,
            "binary" => Self::Binary,
            _ => Self::Node,
        }
    }
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// How a connected server is dialled.
///
/// [`Self::dispatch_kind`] is what gets persisted in the install row's
/// `transport` column, so the two strings it returns are a storage format, not
/// a display detail. They are pinned by test for that reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "`snake_case`")]
#[non_exhaustive]
pub enum Transport {
    /// A local subprocess speaking JSON-RPC over stdin and stdout.
    Stdio,
    /// A remote HTTPS endpoint speaking Streamable HTTP.
    HttpRemote {
        /// The endpoint to dial.
        url: String,
    },
}

impl Transport {
    /// The stable identifier persisted in the install row.
    #[must_use]
    pub fn dispatch_kind(&self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::HttpRemote { .. } => "http_remote",
        }
    }

    /// The inverse of [`Self::dispatch_kind`], for re-hydrating a stored row.
    ///
    /// An unknown or empty kind becomes [`Self::Stdio`]. That is the
    /// migration-safety hatch: rows written before the column existed were all
    /// stdio installs, and a misconfigured row should stall on connect rather
    /// than get misrouted to a transport it was never meant for.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp_bus::Transport;
    /// assert_eq!(Transport::parse("", None), Transport::Stdio);
    /// assert_eq!(
    ///     Transport::parse("http_remote", Some("https://x.test/mcp")),
    ///     Transport::HttpRemote { url: "https://x.test/mcp".into() },
    /// );
    /// ```
    #[must_use]
    pub fn parse(kind: &str, deployment_url: Option<&str>) -> Self {
        match kind {
            "http_remote" => Self::HttpRemote {
                url: deployment_url.unwrap_or_default().to_string(),
            },
            _ => Self::Stdio,
        }
    }

    /// The endpoint for an HTTP-remote install, or `None` for stdio.
    ///
    /// The store persists this as its own column beside the kind.
    #[must_use]
    pub fn deployment_url(&self) -> Option<&str> {
        match self {
            Self::Stdio => None,
            Self::HttpRemote { url } => Some(url.as_str()),
        }
    }
}

/// The serde default for [`InstalledServer::transport`].
fn default_transport() -> Transport {
    Transport::Stdio
}

/// The serde default for [`InstalledServer::enabled`].
const fn default_enabled() -> bool {
    true
}

// ---------------------------------------------------------------------------
// InstalledServer
// ---------------------------------------------------------------------------

/// One server the user has installed.
///
/// # Environment values are not here
///
/// Only the *names* of required environment variables are carried
/// ([`Self::env_keys`]). The values live in their own store and never appear in
/// a list or status payload, because those payloads are logged, returned over
/// RPC, and rendered in user interfaces — and a credential that is never in the
/// struct cannot leak from any of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledServer {
    /// A stable identifier generated at install time.
    pub server_id: String,
    /// The registry's qualified name, such as `@modelcontextprotocol/server-filesystem`.
    pub qualified_name: String,
    /// The registry's display name.
    pub display_name: String,
    /// The registry's short description.
    #[serde(default)]
    pub description: Option<String>,
    /// The registry's icon URL.
    #[serde(default)]
    pub icon_url: Option<String>,
    /// How to launch the subprocess, for stdio installs.
    ///
    /// HTTP-remote installs still carry a value; callers route off
    /// [`Self::transport`] rather than reading this.
    pub command_kind: CommandKind,
    /// The launcher or binary. Empty for HTTP-remote installs.
    pub command: String,
    /// Arguments to [`Self::command`]. Empty for HTTP-remote installs.
    #[serde(default)]
    pub args: Vec<String>,
    /// The names of the environment variables this server requires.
    ///
    /// Names only — see the note on this type.
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// An opaque configuration blob the server was installed with.
    #[serde(default)]
    pub config: Option<Value>,
    /// When the server was installed, in Unix epoch milliseconds.
    pub installed_at: i64,
    /// When the server last connected successfully, in Unix epoch milliseconds.
    #[serde(default)]
    pub last_connected_at: Option<i64>,
    /// How this server is dialled.
    ///
    /// Defaults to [`Transport::Stdio`] so rows persisted before the field
    /// existed still load.
    #[serde(default = "default_transport")]
    pub transport: Transport,
    /// Whether to bring this server up at boot and expose it.
    ///
    /// Turning it off keeps the install row and its stored credentials, so a
    /// user can re-enable without re-entering anything. Defaults to `true` so
    /// rows persisted before the field existed still load as enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// McpTool
// ---------------------------------------------------------------------------

/// A tool exposed by a connected server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    /// The tool's programmatic name.
    pub name: String,
    /// The tool's description.
    ///
    /// Untrusted remote text. Sanitize with [`crate::sanitize`] before placing
    /// it in a model's context.
    #[serde(default)]
    pub description: Option<String>,
    /// The JSON Schema describing the tool's arguments.
    #[serde(default)]
    pub input_schema: Value,
}

impl McpTool {
    /// Builds a tool from its name, with no description and a null schema.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            input_schema: Value::Null,
        }
    }
}

/// One connected server's identity and advertised tools.
///
/// This is what a host needs to describe its available servers — in an
/// orchestrator prompt, say — without reading the store or the configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectedServerOverview {
    /// The install's identifier.
    pub server_id: String,
    /// The registry's qualified name.
    pub qualified_name: String,
    /// The registry's display name.
    pub display_name: String,
    /// The registry's short description — usually the best one-line capability
    /// hint a host can show.
    #[serde(default)]
    pub description: Option<String>,
    /// The tools the server advertises.
    ///
    /// Kept in full so a host can fall back to a tool count when a server has
    /// no description, and so a caller that wants the whole list has it.
    #[serde(default)]
    pub tools: Vec<McpTool>,
}

// ---------------------------------------------------------------------------
// Connection status
// ---------------------------------------------------------------------------

/// Where one installed server stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ServerStatus {
    /// Not connected, and not trying to be.
    Disconnected,
    /// A connection attempt is in flight.
    Connecting,
    /// Connected, with tools available.
    Connected,
    /// Reachable, but it answered 401.
    ///
    /// Distinct from [`Self::Error`] on purpose: the server works and the user
    /// needs to authenticate, so a caller can offer a sign-in path instead of
    /// showing a failure.
    Unauthorized,
    /// A connection attempt failed for some other reason.
    Error,
    /// Installed but switched off by the user.
    Disabled,
}

impl ServerStatus {
    /// The stable string this status is transmitted as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Unauthorized => "unauthorized",
            Self::Error => "error",
            Self::Disabled => "disabled",
        }
    }
}

/// Why a server answered 401, refined enough for a caller to act on.
///
/// # Why this is typed rather than a string
///
/// These three codes drive which affordance a user is offered, and getting one
/// wrong sends them down a path that cannot work — telling someone their token
/// is wrong when the server only accepts OAuth, for instance. As a `String` the
/// set was open, spelled in one place and matched in another, with nothing
/// checking the two agreed. The wire form is unchanged: each variant
/// serializes to exactly the code it replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "`snake_case`")]
#[non_exhaustive]
pub enum McpAuthHint {
    /// The server advertised OAuth. A pasted static token will not work; the
    /// user has to sign in.
    OauthRequired,
    /// A credential was sent and the server refused it — wrong, or expired.
    TokenRejected,
    /// Authentication is required and nothing was supplied yet.
    CredentialRequired,
}

impl McpAuthHint {
    /// The stable code this hint is transmitted as.
    #[must_use]
    pub const fn as_code(self) -> &'static str {
        match self {
            Self::OauthRequired => "oauth_required",
            Self::TokenRejected => "token_rejected",
            Self::CredentialRequired => "credential_required",
        }
    }

    /// Decides which hint a 401 deserves.
    ///
    /// `oauth_advertised` dominates: a server that only accepts OAuth commonly
    /// answers 401 to a pasted static bearer token, and reporting "token
    /// rejected" there would send the user to fix a credential that was never
    /// going to be accepted. The action that works is signing in.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp_bus::McpAuthHint;
    /// // OAuth wins even when a credential was supplied.
    /// assert_eq!(McpAuthHint::classify(true, true), McpAuthHint::OauthRequired);
    /// assert_eq!(McpAuthHint::classify(false, true), McpAuthHint::TokenRejected);
    /// assert_eq!(
    ///     McpAuthHint::classify(false, false),
    ///     McpAuthHint::CredentialRequired,
    /// );
    /// ```
    #[must_use]
    pub const fn classify(oauth_advertised: bool, has_credential: bool) -> Self {
        if oauth_advertised {
            Self::OauthRequired
        } else if has_credential {
            Self::TokenRejected
        } else {
            Self::CredentialRequired
        }
    }
}

/// A per-server status summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnStatus {
    /// The install's identifier.
    pub server_id: String,
    /// The registry's qualified name.
    pub qualified_name: String,
    /// The registry's display name.
    pub display_name: String,
    /// Where the server stands.
    pub status: ServerStatus,
    /// How many tools it advertises.
    pub tool_count: u32,
    /// The most recent connection failure, when there was one.
    pub last_error: Option<String>,
    /// Why a [`ServerStatus::Unauthorized`] happened.
    ///
    /// `None` for every other status. Only the code crosses the wire — never
    /// the raw 401 body or the OAuth metadata URL, both of which can carry
    /// details a caller has no business rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_hint: Option<McpAuthHint>,
}

// ---------------------------------------------------------------------------
// Upstream registry DTOs
// ---------------------------------------------------------------------------

/// A server summary from an upstream registry's list endpoint.
///
/// # Two spellings in, one spelling out
///
/// Smithery sends `camelCase` and the official-registry adapter builds
/// `snake_case`, so deserialization accepts both through aliases. Serialization
/// always produces `snake_case`, which is what a host's own consumers expect.
/// Both directions are pinned by test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryServerSummary {
    /// The registry's qualified name.
    #[serde(alias = "qualifiedName")]
    pub qualified_name: String,
    /// The registry's display name.
    #[serde(alias = "displayName")]
    pub display_name: String,
    /// The registry's short description.
    #[serde(default)]
    pub description: Option<String>,
    /// The registry's icon URL.
    #[serde(default, alias = "iconUrl")]
    pub icon_url: Option<String>,
    /// How many installs the registry reports.
    #[serde(default, alias = "useCount")]
    pub use_count: u64,
    /// Whether the registry hosts a running deployment.
    #[serde(default, alias = "isDeployed")]
    pub is_deployed: bool,
    /// Which upstream this row came from: `smithery` or `mcp_official`.
    ///
    /// Set by the dispatcher so a caller can attribute a row and an install can
    /// route its detail lookup back to the right upstream.
    #[serde(default)]
    pub source: String,
    /// Whether this is the canonical first-party server for a known service.
    ///
    /// Set by the dispatcher from its curation list; never trusted from the
    /// wire.
    #[serde(default)]
    pub official: bool,
    /// The vendor or project URL the server declares.
    ///
    /// A trust signal: the strict catalog filter requires it. Set by the
    /// adapter and **never deserialized**, so an upstream that starts emitting
    /// the key cannot use it to pass curation.
    #[serde(default, skip_deserializing)]
    pub website_url: Option<String>,
    /// The static credential the server declares needing, when it declares one.
    ///
    /// `Some("api_key")` when the registry metadata names a secret header or
    /// environment variable; `None` when the server is open, OAuth-only, or
    /// simply under-specified. Set by the adapter and **never deserialized**,
    /// for the same reason as [`Self::website_url`].
    #[serde(default, skip_deserializing)]
    pub auth_kind: Option<String>,
    /// Anything else the upstream sent.
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// A server detail record from an upstream registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryServerDetail {
    /// The registry's qualified name.
    #[serde(alias = "qualifiedName")]
    pub qualified_name: String,
    /// The registry's display name.
    #[serde(alias = "displayName")]
    pub display_name: String,
    /// The registry's short description.
    #[serde(default)]
    pub description: Option<String>,
    /// The registry's icon URL.
    #[serde(default, alias = "iconUrl")]
    pub icon_url: Option<String>,
    /// The connection types this server offers.
    #[serde(default)]
    pub connections: Vec<RegistryConnection>,
    /// Which upstream this row came from: `smithery` or `mcp_official`.
    #[serde(default)]
    pub source: String,
    /// Anything else the upstream sent.
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// One connection type listed on a server detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryConnection {
    /// The connection type: `stdio` or `http`.
    pub r#type: String,
    /// The endpoint, for an HTTP connection.
    #[serde(default, alias = "deploymentUrl")]
    pub deployment_url: Option<String>,
    /// The JSON Schema for this connection's configuration.
    #[serde(default, alias = "configSchema")]
    pub config_schema: Option<Value>,
    /// An example configuration the registry supplies.
    #[serde(default, alias = "exampleConfig")]
    pub example_config: Option<Value>,
    /// Whether the registry considers this connection published.
    #[serde(default)]
    pub published: bool,
    /// Anything else the upstream sent.
    #[serde(flatten, default)]
    pub extra: ExtraFields,
}

/// The pagination block on a registry list response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryPagination {
    /// The page this response covers, one-based.
    #[serde(default, alias = "currentPage")]
    pub current_page: u32,
    /// How many rows a page holds.
    #[serde(default, alias = "pageSize")]
    pub page_size: u32,
    /// How many pages there are.
    #[serde(default, alias = "totalPages")]
    pub total_pages: u32,
    /// How many rows there are across every page.
    #[serde(default, alias = "totalCount")]
    pub total_count: u64,
}

/// A registry list response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegistryListResponse {
    /// The rows on this page.
    #[serde(default)]
    pub servers: Vec<RegistryServerSummary>,
    /// Where this page sits in the whole result.
    #[serde(default)]
    pub pagination: RegistryPagination,
}

// ---------------------------------------------------------------------------
// Conversation
// ---------------------------------------------------------------------------

/// One turn of the setup conversation a host runs to help configure a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurn {
    /// Who spoke: conventionally `user` or `assistant`.
    pub role: String,
    /// What they said.
    pub content: String,
}

impl ChatTurn {
    /// Builds a turn from a role and its content.
    #[must_use]
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}
