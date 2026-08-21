//! Talking to a remote MCP server.
//!
//! Two transports, one protocol. [`http::McpHttpClient`] speaks Streamable HTTP
//! with OAuth discovery and server-sent events; a subprocess transport speaking
//! newline-delimited JSON-RPC joins it in the same directory. Both negotiate
//! from [`tinymcp_bus::SUPPORTED_PROTOCOL_VERSIONS`], and both render results
//! through [`render_tool_result`], so a caller sees one vocabulary regardless of
//! how the server was reached.
//!
//! # Endpoints are redacted before they are logged
//!
//! [`redact_endpoint`] reduces a URL to its scheme and authority, and returns
//! `<redacted>` outright for anything carrying userinfo or an unexpected
//! scheme. Every log line and every error message in this crate passes an
//! endpoint through it first. MCP endpoints routinely carry an API key in a
//! query parameter and occasionally credentials in userinfo, and errors reach
//! logs, telemetry, and user interfaces alike.

pub(crate) mod http;

use serde_json::Value;
use tinymcp_bus::{McpToolResult, SUPPORTED_PROTOCOL_VERSIONS};

use crate::error::{Error, Result};

/// Reduces an endpoint to scheme and authority, or `<redacted>`.
///
/// Returns `<redacted>` when the URL carries userinfo (anything before an `@`
/// in the authority), when the scheme is neither `http` nor `https`, or when
/// there is no authority at all. Everything after the authority — path, query,
/// fragment — is dropped unconditionally, because that is where MCP servers
/// most often carry an API key.
///
/// # Examples
///
/// ```
/// # use tinymcp::redact_endpoint;
/// assert_eq!(
///     redact_endpoint("https://example.test/mcp?key=secret"),
///     "https://example.test",
/// );
/// assert_eq!(redact_endpoint("https://user:pass@example.test/mcp"), "<redacted>");
/// assert_eq!(redact_endpoint("file:///etc/passwd"), "<redacted>");
/// ```
#[must_use]
pub fn redact_endpoint(raw: &str) -> String {
    const REDACTED: &str = "<redacted>";

    let trimmed = raw.trim();
    let (scheme, rest) = if let Some(rest) = trimmed.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        ("http", rest)
    } else {
        return REDACTED.to_string();
    };

    // `split` always yields at least one item, so this cannot be empty for a
    // non-empty `rest`; the default covers `rest` being empty.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return REDACTED.to_string();
    }
    format!("{scheme}://{authority}")
}

/// Renders a raw `tools/call` reply into the shape a caller consumes.
///
/// Text blocks are concatenated, separated by blank lines. A reply carrying no
/// text at all is rendered as its own JSON, so a structured-only result still
/// says something rather than nothing.
///
/// A reply flagged `isError` becomes an error *result*, not an error return: the
/// call succeeded and the tool said no, and a caller that conflates the two
/// reports a network problem for a bad argument.
///
/// # Examples
///
/// ```
/// # use tinymcp::render_tool_result;
/// let rendered = render_tool_result(&serde_json::json!({
///     "content": [{ "type": "text", "text": "sunny" }],
/// }));
/// assert!(!rendered.is_error);
/// assert_eq!(rendered.text(), "sunny");
/// ```
#[must_use]
pub fn render_tool_result(result: &Value) -> McpToolResult {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut rendered = String::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for block in content {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                if !rendered.is_empty() {
                    rendered.push_str("\n\n");
                }
                rendered.push_str(text);
            }
        }
    }
    if rendered.is_empty() {
        rendered = result.to_string();
    }

    if is_error {
        McpToolResult::error(rendered)
    } else {
        McpToolResult::success(rendered)
    }
}

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
mod test;
