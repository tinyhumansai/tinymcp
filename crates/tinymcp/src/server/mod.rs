//! Serving the Model Context Protocol.
//!
//! The client half of this crate talks *to* MCP servers; this half lets a host
//! *be* one. The host implements [`McpServerHandler`] — its identity, its
//! tools, its resources — and this module does the protocol around it.
//!
//! See `README.md` beside this file for the design and the wire guarantees.

pub mod args;
pub mod bridge;
#[cfg(test)]
mod fixture;
#[cfg(feature = "server-http")]
mod http;
mod protocol;
mod session;
mod stdio;
mod types;

#[cfg(feature = "server-http")]
pub use http::{HttpServerConfig, run_http, run_http_reporting};
pub use protocol::{handle_line, handle_value};
pub use session::ClientSession;
pub use stdio::run_stdio;
pub use types::{
    DEFAULT_SOURCE_TYPE_PREFIX, McpServerHandler, RequestContext, RequestHeaders, ResourceSpec,
    ServerInfo, ServerToolSpec, ToolCallError,
};

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;

mod context;
mod headers;
mod resource_spec;
mod tool_spec;
pub use context::RequestContextExt;
pub use headers::RequestHeadersExt;
pub use resource_spec::ResourceSpecExt;
pub use tool_spec::ServerToolSpecExt;
