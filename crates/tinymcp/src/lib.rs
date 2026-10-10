//! A Model Context Protocol client, packaged as an installable `TinyBus`
//! module.
//!
//! This crate knows how to *talk to* MCP servers. It dials them over Streamable
//! HTTP or as a subprocess, browses the upstream registries, keeps track of what
//! a user installed, supervises what it spawned, and records what got written.
//!
//! It also lets a host *be* one: [`server`] implements the protocol and the
//! stdio transport around a host's [`McpServerHandler`], and — with the
//! `server-http` feature — the Streamable HTTP + SSE transport.
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
pub mod server;
#[cfg(all(feature = "module", not(feature = "static-link")))]
mod tinybus_module;
#[cfg(feature = "static-link")]
pub mod tinybus_module;
#[cfg(feature = "tools")]
pub mod tools;
pub mod transport;
#[cfg(feature = "ui")]
pub mod ui;

pub use audit::AuditStore;
pub use config_servers::{
    McpRegistrySource, McpServerDefinition, McpServerRegistry, McpTransportClient,
};
pub use error::{Error, Result};
pub use registry::{
    AppliedServer, ConfigApplyReport, Connections, McpRegistry, OAuthFlow, ProbeOutcome,
    REMOTE_REQUEST_TIMEOUT, SecretRef, SecretVault, ServerRef, Store, SupervisedHost, Supervisor,
    SupervisorConfig, SupervisorEvent, TickReport,
};
pub use server::{
    ClientSession, McpServerHandler, RequestContext, RequestHeaders, ResourceSpec, ServerInfo,
    ServerToolSpec, ToolCallError, run_stdio,
};
#[cfg(feature = "server-http")]
pub use server::{HttpServerConfig, run_http, run_http_reporting};
#[cfg(feature = "module")]
pub use tinybus_module::{McpService, ModuleConfig};
pub use transport::http::{McpHttpClient, McpHttpClientBuilder};
pub use transport::stdio::McpStdioClient;
pub use transport::{redact_endpoint, render_tool_result};

// The wire contract, re-exported by module rather than by item so every path
// through this crate resolves to the same definitions the contract crate
// publishes. A host may depend on `tinymcp-bus` directly and get exactly these
// types; nothing here redefines them.
pub use tinymcp_bus;
pub use tinymcp_bus::{
    AuthDetection, AuthKind, AuthorizationServerMetadata, CONTRACT_VERSION, ChatTurn, CommandKind,
    ConnStatus, ConnectedServerOverview, DEFAULT_LIST_LIMIT, ERROR_MESSAGE_MAX_BYTES, ExtraFields,
    HEADER_PROTOCOL_VERSION, HEADER_SESSION_ID, HttpHeader, INTERFACE, InstalledServer,
    LATEST_PROTOCOL_VERSION, MAX_DESCRIPTION_BYTES, MAX_LIST_LIMIT, MAX_RESOURCE_BYTES,
    MAX_TITLE_BYTES, MCP_CALL_RESULT_KIND, MCP_RESULT_KIND, METHODS, McpAuthChallenge,
    McpAuthConfig, McpAuthHint, McpAuthorizationContext, McpCallError, McpCallOutcome,
    McpClientConfig, McpClientIdentityConfig, McpClientInfo, McpInitializeResult, McpProxyConfig,
    McpRegistryAuthConfig, McpRemoteTool, McpResource, McpResourceContents, McpResultEnvelope,
    McpServerConfig, McpServerToolResult, McpSseEvent, McpTool, McpToolContent, McpToolResult,
    McpWriteListQuery, McpWriteRecord, NewMcpWriteRecord, OBJECT_PATH, ProtectedResourceMetadata,
    RegistryConnection, RegistryFreshness, RegistryListResponse, RegistryPagination,
    RegistryServerDetail, RegistryServerSummary, SUPPORTED_PROTOCOL_VERSIONS, SearchCuration,
    ServerDetail, ServerStatus, Transport, config, is_compatible, names, sanitize, version,
};
