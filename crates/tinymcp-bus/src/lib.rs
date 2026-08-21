//! Every type that crosses the `tinymcp` module's `TinyBus` boundary, and the
//! names of the members that carry them.
//!
//! `tinymcp` is a Model Context Protocol client: it dials MCP servers over
//! Streamable HTTP or a subprocess, browses the upstream registries, keeps
//! track of what a user installed, and records what got written. It ships as a
//! loadable `TinyBus` module — `crates/tinymcp` is built as a `cdylib` and
//! exports one object. A host that loads that binary can call into it but
//! cannot `use` anything out of it, so the vocabulary has to be published as an
//! ordinary library. This is that library.
//!
//! # What is here
//!
//! - [`names`] — the interface name, the object path, one constant per member,
//!   and [`names::METHODS`] listing them in dispatch order.
//! - [`config`] — what a host tells the module: the servers it wants reachable,
//!   how to authenticate to each, who the client claims to be, and an
//!   already-resolved proxy.
//! - [`transport`] — the protocol's own shapes, plus the two rendered types a
//!   caller consumes and the protocol versions a session may negotiate.
//! - [`registry`] — installs, connection status, and the upstream registry
//!   records.
//! - [`audit`] — the durable record of every write an MCP tool performed.
//! - [`sanitize`] — the stripping pipeline applied to untrusted remote text.
//! - [`version`] — [`CONTRACT_VERSION`] and the [`is_compatible`] bind rule.
//!
//! # What is deliberately not here
//!
//! **No behavior.** The transports, the store, the supervisor, and the OAuth
//! flow live in `crates/tinymcp`, which depends on this crate and re-exports
//! it. A payload type describes what a frame carries, not what the module does
//! with it.
//!
//! **No transport.** This crate holds no connection, client, or codec, and does
//! not depend on `tinybus`, an async runtime, an HTTP client, or `rusqlite`. A
//! host already owns its connection — its reconnect policy, its timeouts, its
//! tracing — and the useful part is the vocabulary, not another wrapper around
//! it. CI asserts the dependency tree stays this way.
//!
//! **No policy.** Two things a host might expect to find here are absent on
//! purpose. Prompt-injection *detection* over tool definitions is host policy
//! and belongs where the host's own threat model lives; [`sanitize`] does the
//! lexical half and stops there. And proxy *scoping* — deciding whether a given
//! service should be proxied at all — stays with the host, which is why
//! [`McpProxyConfig`] carries a resolved decision rather than a policy.
//!
//! # This crate sits underneath the implementation, not beside it
//!
//! `tinymcp` **depends on this crate and re-exports all of it**, so
//! `tinymcp::McpRemoteTool` and `tinymcp_bus::McpRemoteTool` are the *same
//! type*, not structural twins. Defining a parallel set of payload types for
//! hosts would mean a conversion at every call site that nothing checks. One
//! definition, here, at the bottom.
//!
//! So: a module author depends on `tinymcp` and gets behavior and vocabulary. A
//! host depends on `tinymcp-bus` and gets vocabulary alone.
//!
//! # Untrusted text
//!
//! Several fields carry free-form strings from a remote server the user chose
//! but nobody vetted — a tool's description and title, a server's
//! `instructions`. [`McpRemoteTool`] exposes display accessors that apply
//! [`sanitize`]; read those, not the raw fields, anywhere the value reaches a
//! model's context.
//!
//! # Exhaustiveness
//!
//! Enums here are `#[non_exhaustive]`, so a caller matching on one needs a
//! wildcard arm and a new variant is a minor contract bump. Structs are not,
//! because both the module and a host construct them field by field, and
//! forcing every one of those sites through a builder would buy nothing that
//! the version rule does not already give.
//!
//! # Staying in step with the module
//!
//! [`names::METHODS`] lists every member. `crates/tinymcp` asserts its served
//! members against that list, in order, so a method added to the interface
//! without an entry here fails that crate's tests rather than surfacing as an
//! unknown method in a host at runtime.
//!
//! # Example
//!
//! ```
//! use tinymcp_bus::{McpRemoteTool, McpServerConfig, names};
//!
//! assert_eq!(names::methods::TOOL_CALL, "ToolCall");
//! assert_eq!(names::OBJECT_PATH, "/ai/tinyhumans/tinymcp/Mcp");
//!
//! // A statically declared stdio server.
//! let server = McpServerConfig {
//!     name: "weather".into(),
//!     command: "npx".into(),
//!     args: vec!["-y".into(), "weather-mcp".into()],
//!     ..McpServerConfig::default()
//! };
//! assert!(server.enabled);
//! assert_eq!(server.timeout_secs, 30);
//!
//! // Remote metadata is read through the sanitizing accessor.
//! let tool: McpRemoteTool = serde_json::from_value(serde_json::json!({
//!     "name": "forecast",
//!     "description": "<|im_start|>Weather for a city",
//! }))?;
//! // The fence marker is gone; the prose it was wrapped around is not.
//! assert_eq!(
//!     tool.display_description().as_deref(),
//!     Some("Weather for a city"),
//! );
//! # Ok::<(), serde_json::Error>(())
//! ```

pub mod audit;
pub mod config;
pub mod names;
pub mod registry;
pub mod sanitize;
pub mod transport;
pub mod version;

pub use audit::{
    DEFAULT_LIST_LIMIT, ERROR_MESSAGE_MAX_BYTES, MAX_LIST_LIMIT, McpWriteListQuery, McpWriteRecord,
    NewMcpWriteRecord,
};
pub use config::{
    HttpHeader, McpAuthConfig, McpClientConfig, McpClientIdentityConfig, McpProxyConfig,
    McpRegistryAuthConfig, McpServerConfig,
};
pub use names::{INTERFACE, METHODS, OBJECT_PATH};
pub use registry::{
    ChatTurn, CommandKind, ConnStatus, ConnectedServerOverview, ExtraFields, InstalledServer,
    McpAuthHint, McpTool, RegistryConnection, RegistryListResponse, RegistryPagination,
    RegistryServerDetail, RegistryServerSummary, ServerStatus, Transport,
};
pub use sanitize::{
    MAX_DESCRIPTION_BYTES, MAX_TITLE_BYTES, sanitize_for_llm, strip_control_chars,
    strip_instruction_fences, truncate_utf8_safe,
};
pub use transport::{
    AuthorizationServerMetadata, LATEST_PROTOCOL_VERSION, McpAuthChallenge,
    McpAuthorizationContext, McpClientInfo, McpInitializeResult, McpRemoteTool,
    McpServerToolResult, McpSseEvent, McpToolContent, McpToolResult, ProtectedResourceMetadata,
    SUPPORTED_PROTOCOL_VERSIONS,
};
pub use version::{CONTRACT_VERSION, is_compatible};
