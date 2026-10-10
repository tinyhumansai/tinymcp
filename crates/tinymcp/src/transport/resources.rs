//! Decoding `resources/list` and `resources/read` replies.
//!
//! Shared by both transports so a server reached over HTTP and one reached
//! over stdio are read by the same rules, including the size cap.

use serde_json::Value;
use tinymcp_bus::{MAX_RESOURCE_BYTES, McpResource, McpResourceContents};

use crate::error::{Error, Result};

/// The most `resources/list` pages a listing follows before stopping.
pub(crate) const MAX_RESOURCE_PAGES: usize = 32;

/// One `resources/list` page: its resources and the cursor for the next.
pub(crate) fn parse_resource_page(result: &Value) -> Result<(Vec<McpResource>, Option<String>)> {
    let resources = result
        .get("resources")
        .ok_or_else(|| Error::malformed("resources/list reply has no `resources` member"))?;
    let resources: Vec<McpResource> = serde_json::from_value(resources.clone())
        .map_err(|error| Error::malformed(format!("resources/list entries: {error}")))?;
    let next = result
        .get("nextCursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
        .map(ToString::to_string);
    Ok((resources, next))
}

/// The contents of a `resources/read` reply for `uri`.
///
/// # Errors
///
/// [`Error::MalformedResponse`] when the reply has no `contents` array, and
/// [`Error::ResourceTooLarge`] when the contents together exceed
/// [`MAX_RESOURCE_BYTES`].
pub(crate) fn parse_read_result(uri: &str, result: &Value) -> Result<Vec<McpResourceContents>> {
    let contents = result
        .get("contents")
        .ok_or_else(|| Error::malformed("resources/read reply has no `contents` member"))?;
    let contents: Vec<McpResourceContents> = serde_json::from_value(contents.clone())
        .map_err(|error| Error::malformed(format!("resources/read contents: {error}")))?;
    let bytes: usize = contents.iter().map(McpResourceContents::byte_len).sum();
    if bytes > MAX_RESOURCE_BYTES {
        return Err(Error::ResourceTooLarge {
            uri: uri.to_string(),
            bytes,
            limit: MAX_RESOURCE_BYTES,
        });
    }
    Ok(contents)
}

/// The params of a `resources/list` request for `cursor`.
pub(crate) fn list_params(cursor: Option<&str>) -> Value {
    cursor.map_or_else(
        || serde_json::json!({}),
        |cursor| serde_json::json!({ "cursor": cursor }),
    )
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod test;
