//! Talking to a remote MCP server.
//!
//! Two transports, one protocol. [`http::McpHttpClient`] speaks Streamable HTTP
//! with OAuth discovery and server-sent events; [`stdio::McpStdioClient`] spawns
//! a server and speaks newline-delimited JSON-RPC to it. Both negotiate
//! from [`tinymcp_bus::SUPPORTED_PROTOCOL_VERSIONS`], and both render results
//! through [`render_tool_result`], so a caller sees one vocabulary regardless of
//! how the server was reached.
//!
//! # Endpoints are redacted before they are logged
//!
//! [`redact_endpoint`], defined here so a host can call the same
//! function, reduces a URL to its scheme and authority, and returns
//! `<redacted>` outright for anything carrying userinfo or an unexpected
//! scheme. Every log line and every error message in this crate passes an
//! endpoint through it first. MCP endpoints routinely carry an API key in a
//! query parameter and occasionally credentials in userinfo, and errors reach
//! logs, telemetry, and user interfaces alike.

mod render;
mod resources;
#[cfg(test)]
pub(crate) mod ui_fixture;
pub use render::{redact_endpoint, render_tool_result};

pub mod http;
pub mod stdio;

use tinymcp_bus::SUPPORTED_PROTOCOL_VERSIONS;

use crate::error::{Error, Result};

/// Checks that a server's negotiated protocol version is one this client speaks.
///
/// # Errors
///
/// Returns [`Error::UnsupportedProtocolVersion`] when it is not. Proceeding
/// against an unknown version would mean guessing at framing that has never
/// been exercised, which fails later and less clearly than failing here.
pub(crate) fn validate_protocol_version(version: &str) -> Result<()> {
    if SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
        Ok(())
    } else {
        Err(Error::UnsupportedProtocolVersion {
            version: version.to_string(),
        })
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
