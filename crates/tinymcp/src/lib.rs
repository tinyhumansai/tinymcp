//! A Model Context Protocol client, packaged as an installable `TinyBus`
//! module.
//!
//! This crate knows how to *talk to* MCP servers. It dials them over Streamable
//! HTTP or as a subprocess, browses the upstream registries, keeps track of what
//! a user installed, supervises what it spawned, and records what got written.
//!
//! # Layout
//!
//! This is the implementation half of a two-crate workspace:
//!
//! - [`tinymcp_bus`] — the wire contract. Member names, payload types, and the
//!   contract version, with no transport and no behavior. A host that only
//!   makes calls depends on that crate alone and compiles neither this crate
//!   nor `tinybus`.
//! - `tinymcp` — this crate. The transports, the registry, the audit log, and
//!   the `TinyBus` adapter that serves them, built as both an `rlib` and the
//!   `cdylib` the loader consumes.
//!
//! Every public item from the contract is re-exported here, so
//! `tinymcp::McpRemoteTool` is the *same type* as `tinymcp_bus::McpRemoteTool`
//! rather than a structural twin, and a caller takes one dependency instead of
//! two.
//!
//! # Untrusted input is the design constraint
//!
//! Everything this crate talks to was chosen by a user and vetted by nobody: an
//! arbitrary HTTPS endpoint, or an arbitrary subprocess launched through `npx`
//! or `uvx`. Three rules follow, and they are worth knowing before reading any
//! of the code.
//!
//! **Remote text is sanitized before it can reach a model.** Tool descriptions
//! and titles are read through the display accessors on
//! [`McpRemoteTool`], which apply [`tinymcp_bus::sanitize`].
//!
//! **Endpoints are redacted before they are logged.** [`redact_endpoint`]
//! reduces a URL to scheme and authority and refuses anything carrying
//! userinfo. MCP endpoints routinely carry an API key in a query parameter.
//!
//! **Tool permission is enforced before the transport.** A denied tool never
//! reaches the network or a subprocess.
//!
//! # Errors
//!
//! Every fallible public function returns [`Result`], the crate alias over
//! [`Error`]. One variant is worth singling out: [`Error::Unauthorized`] means
//! the server is reachable and wants credentials, which is a state a caller
//! acts on rather than reports. Match on it with [`Error::is_unauthorized`]
//! rather than reading a message.
//!
//! # Example
//!
//! ```
//! use tinymcp::{redact_endpoint, render_tool_result};
//!
//! // An endpoint is never logged raw.
//! assert_eq!(
//!     redact_endpoint("https://example.test/mcp?api_key=secret"),
//!     "https://example.test",
//! );
//!
//! // A raw `tools/call` reply renders into the shape a caller consumes.
//! let rendered = render_tool_result(&serde_json::json!({
//!     "content": [{ "type": "text", "text": "sunny, 21C" }],
//! }));
//! assert!(!rendered.is_error);
//! assert_eq!(rendered.text(), "sunny, 21C");
//! ```

pub mod audit;
pub mod config_servers;
mod error;
pub mod registry;
#[cfg(feature = "module")]
mod tinybus_module;
pub mod transport;

pub use audit::AuditStore;
pub use config_servers::{
    McpRegistrySource, McpServerDefinition, McpServerRegistry, McpTransportClient,
};
pub use error::{Error, Result};
pub use registry::{
    AuthDetection, AuthKind, Connections, McpRegistry, OAuthFlow, ProbeOutcome,
    REMOTE_REQUEST_TIMEOUT, SecretRef, SecretVault, ServerRef, Store, Supervisor, SupervisorConfig,
    SupervisorEvent, TickReport,
};
#[cfg(feature = "module")]
pub use tinybus_module::{McpService, ModuleConfig, ServerDetail};
pub use transport::http::{McpHttpClient, McpHttpClientBuilder};
pub use transport::stdio::McpStdioClient;
pub use transport::{redact_endpoint, render_tool_result};

// The wire contract, re-exported by module rather than by item so every path
// through this crate resolves to the same definitions the contract crate
// publishes. A host may depend on `tinymcp-bus` directly and get exactly these
// types; nothing here redefines them.
pub use tinymcp_bus;
pub use tinymcp_bus::{
    AuthorizationServerMetadata, CONTRACT_VERSION, ChatTurn, CommandKind, ConnStatus,
    ConnectedServerOverview, DEFAULT_LIST_LIMIT, ERROR_MESSAGE_MAX_BYTES, ExtraFields, HttpHeader,
    INTERFACE, InstalledServer, LATEST_PROTOCOL_VERSION, MAX_DESCRIPTION_BYTES, MAX_LIST_LIMIT,
    MAX_TITLE_BYTES, METHODS, McpAuthChallenge, McpAuthConfig, McpAuthHint,
    McpAuthorizationContext, McpClientConfig, McpClientIdentityConfig, McpClientInfo,
    McpInitializeResult, McpProxyConfig, McpRegistryAuthConfig, McpRemoteTool, McpServerConfig,
    McpServerToolResult, McpSseEvent, McpTool, McpToolContent, McpToolResult, McpWriteListQuery,
    McpWriteRecord, NewMcpWriteRecord, OBJECT_PATH, ProtectedResourceMetadata, RegistryConnection,
    RegistryListResponse, RegistryPagination, RegistryServerDetail, RegistryServerSummary,
    SUPPORTED_PROTOCOL_VERSIONS, ServerStatus, Transport, config, is_compatible, names, sanitize,
    version,
};
