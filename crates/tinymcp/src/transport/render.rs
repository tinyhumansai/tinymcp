//! Rendering a tool reply, and redacting an endpoint before it is logged.
//!
//! Both are pure functions over their arguments, which is why they live in the
//! contract crate: a host that logs an endpoint or renders a tool reply needs
//! the *same* behavior the module applies, and a reimplementation would drift.
//! They hold no transport, so they cost the host nothing to link.

use serde_json::Value;

use tinymcp_bus::{MAX_RESOURCE_BYTES, McpResourceContents, McpToolResult};

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
/// `structuredContent`, `_meta`, and embedded `resource` blocks are copied
/// into the host-facing fields of [`McpToolResult`]. They never change the
/// rendered text. An embedded resource above [`MAX_RESOURCE_BYTES`] is
/// dropped.
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

    let mut rendered = if is_error {
        McpToolResult::error(rendered)
    } else {
        McpToolResult::success(rendered)
    };
    rendered.structured_content = result
        .get("structuredContent")
        .filter(|value| !value.is_null())
        .cloned();
    rendered.meta = result
        .get("_meta")
        .filter(|value| !value.is_null())
        .cloned();
    rendered.resources = embedded_resources(result);
    rendered
}

/// The `resource` content blocks of a reply, each within
/// [`MAX_RESOURCE_BYTES`].
fn embedded_resources(result: &Value) -> Vec<McpResourceContents> {
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("resource"))
        .filter_map(|block| block.get("resource"))
        .filter_map(|resource| serde_json::from_value::<McpResourceContents>(resource.clone()).ok())
        .filter(|resource| {
            let fits = resource.byte_len() <= MAX_RESOURCE_BYTES;
            if !fits {
                tracing::debug!(
                    bytes = resource.byte_len(),
                    "dropped an embedded resource above the size cap"
                );
            }
            fits
        })
        .collect()
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod test;
