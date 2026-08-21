//! What crosses the wire between a client and a remote MCP server.
//!
//! These are the protocol's own shapes — the `initialize` reply, an advertised
//! tool, a `tools/call` result, an SSE frame, and the OAuth discovery
//! documents — plus the two rendered types a caller actually consumes.
//!
//! # The two protocol-version constants
//!
//! [`LATEST_PROTOCOL_VERSION`] is what a client asks for;
//! [`SUPPORTED_PROTOCOL_VERSIONS`] is what it will accept in reply. They live
//! here rather than in either transport because both transports negotiate from
//! them and a host may want to report them. Before the extraction they were
//! duplicated in the HTTP and stdio clients, which is exactly the drift a
//! single definition exists to prevent.
//!
//! # Untrusted text
//!
//! Several fields here — a tool's `description` and `title`, a server's
//! `instructions` — are free-form strings from a remote peer the user chose but
//! nobody vetted. [`McpRemoteTool`] carries display accessors that apply
//! [`crate::sanitize`]; read those rather than the raw fields anywhere the
//! value reaches a model's context.

mod types;

pub use types::{
    AuthorizationServerMetadata, LATEST_PROTOCOL_VERSION, McpAuthChallenge,
    McpAuthorizationContext, McpClientInfo, McpInitializeResult, McpRemoteTool,
    McpServerToolResult, McpSseEvent, McpToolContent, McpToolResult, ProtectedResourceMetadata,
    SUPPORTED_PROTOCOL_VERSIONS,
};

#[cfg(test)]
mod test;
