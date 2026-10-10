//! Shared MCP wire vocabulary, fixed tool declarations and `TinyBus` member names.
//!
//! The implementation re-exports these exact types. Hosts link this crate for
//! payloads and declarations; protocol, transports, rendering, normalization,
//! filtering and sanitization execute outside the contract. The default and
//! all-feature dependency closures contain only serialization and schema utilities.
//!
//! Untrusted remote text stays raw in these DTOs. Loadable consumers prepare it
//! through `DisplayRemoteTool`, `TransformText`, `NormalizeToolArguments` and
//! `RenderToolOutput`. Library consumers use implementation extension traits;
//! generic lexical helpers live in `TinyTools`. [`sanitize`] holds MCP byte caps.
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
//! // Vocabulary preserves raw remote text. Preparation executes in the module.
//! assert_eq!(
//!     tool.description.as_deref(),
//!     Some("<|im_start|>Weather for a city"),
//! );
//! # Ok::<(), serde_json::Error>(())
//! ```

pub mod agent_tools;
pub mod audit;
pub mod auth;
pub mod config;
pub mod errors;
pub mod method;
pub mod names;
pub mod registry;
pub mod sanitize;
pub mod server;
pub mod supervisor;
pub mod transport;
pub mod ui;
pub mod version;

pub use agent_tools::{
    AgentToolEffect, AgentToolSpec, ArgsError, MCP_CALL_RESULT_KIND, MCP_RESULT_KIND, McpCallError,
    McpCallOutcome, McpResultEnvelope, RegistryTool, registry_tool_specs,
};
pub use audit::{
    DEFAULT_LIST_LIMIT, ERROR_MESSAGE_MAX_BYTES, MAX_LIST_LIMIT, McpWriteListQuery, McpWriteRecord,
    NewMcpWriteRecord,
};
pub use auth::{AuthDetection, AuthKind};
pub use config::{
    HttpHeader, McpAuthConfig, McpClientConfig, McpClientIdentityConfig, McpProxyConfig,
    McpRegistryAuthConfig, McpServerConfig,
};
pub use method::{
    ConnectOutcome, InstallOutcome, RegistryFreshness, RegistrySearchPage, RegistrySettings,
    SearchCuration, ServerDetail, ToolCallOutcome, UpdateEnvOutcome, UpdateEnvStatus,
};
pub use names::{DIRECTORY_OBJECT_PREFIX, INTERFACE, METHODS, OBJECT_PATH};
pub use registry::{
    ChatTurn, CommandKind, ConnStatus, ConnectedServerOverview, ExtraFields, InstalledServer,
    McpAuthHint, McpTool, RegistryConnection, RegistryListResponse, RegistryPagination,
    RegistryServerDetail, RegistryServerSummary, ServerStatus, Transport,
};
pub use sanitize::{MAX_DESCRIPTION_BYTES, MAX_TITLE_BYTES};
pub use transport::{
    AuthorizationServerMetadata, HEADER_PROTOCOL_VERSION, HEADER_SESSION_ID,
    LATEST_PROTOCOL_VERSION, MAX_RESOURCE_BYTES, McpAuthChallenge, McpAuthorizationContext,
    McpClientInfo, McpInitializeResult, McpRemoteTool, McpResource, McpResourceContents,
    McpServerToolResult, McpSseEvent, McpToolContent, McpToolResult, ProtectedResourceMetadata,
    SUPPORTED_PROTOCOL_VERSIONS,
};
pub use ui::{
    LinkClass, MCP_APP_MIME, MCP_APPS_EXTENSION, MCP_UI_KIND, McpUiPresentation, UiCsp, UiFlavor,
    UiLink, UiLinkKind, UiRendering, UiResource, WidgetCallPolicy,
};
pub use version::{CONTRACT_VERSION, is_compatible};

pub use supervisor::{ProbeOutcome, ServerRef, SupervisorBatch, SupervisorEvent, TickReport};

pub use server::{
    ServerCallback, ServerHostCall, ServerHostReply, ServerInput, ServerOperationRef,
    ServerOperationSnapshot, ServerOperationState, ServerSessionConfig,
};

pub use server::{
    DEFAULT_SOURCE_TYPE_PREFIX, RequestContext, RequestHeaders, ResourceSpec, ServerInfo,
    ServerToolSpec, ToolCallError,
};

pub mod processing;
pub use processing::{
    DisplayRemoteToolRequest, MAX_PROCESSING_BYTES, RemoteToolDisplay, RenderToolOutputRequest,
    TextTransform, ToolOutputFormat, TransformTextRequest,
};
