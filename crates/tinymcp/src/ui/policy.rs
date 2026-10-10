//! What a host does with tool UI: whether a widget may call a tool, how a
//! presentation is shown, and what the host advertises to servers.

use serde_json::{Map, Value, json};

use super::{
    MCP_APP_MIME, MCP_APPS_EXTENSION, McpUiPresentation, UiFlavor, UiRendering, WidgetCallPolicy,
};

/// Whether a tool may be called from a widget: its `_meta.ui.visibility`
/// is absent or lists `"app"`.
#[must_use]
pub fn visible_to_app(tool_meta: Option<&Value>) -> bool {
    match tool_meta.and_then(|meta| meta.pointer("/ui/visibility")) {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.iter().any(|item| item.as_str() == Some("app")),
        Some(_) => false,
    }
}

/// Whether a tool declares it changes nothing (`annotations.readOnlyHint`).
#[must_use]
pub fn is_read_only(annotations: Option<&Value>) -> bool {
    annotations
        .and_then(|annotations| annotations.get("readOnlyHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// What a host does with a tool call a widget of `flavor` asks for.
///
/// Host-supplied pages may not call tools, nor may a widget call a tool
/// hidden from apps. A read-only tool is called; anything else waits for the
/// user.
#[must_use]
pub fn widget_call_policy(
    flavor: UiFlavor,
    tool_meta: Option<&Value>,
    annotations: Option<&Value>,
) -> WidgetCallPolicy {
    if flavor == UiFlavor::HostInline || !visible_to_app(tool_meta) {
        WidgetCallPolicy::Deny
    } else if is_read_only(annotations) {
        WidgetCallPolicy::Allow
    } else {
        WidgetCallPolicy::Confirm
    }
}

/// How a host shows `presentation`. A host that cannot sandbox widgets
/// (`renders_widgets` false) falls back to the links.
#[must_use]
pub fn rendering(renders_widgets: bool, presentation: &McpUiPresentation) -> UiRendering {
    if renders_widgets && presentation.has_frame() {
        UiRendering::Widget
    } else if !presentation.links.is_empty() {
        UiRendering::Links
    } else {
        UiRendering::Nothing
    }
}

/// The client capabilities a host that renders widgets advertises. A host
/// that does not leaves this out, so servers answer with text alone.
#[must_use]
pub fn client_capabilities() -> Value {
    let mut extensions = Map::new();
    extensions.insert(
        MCP_APPS_EXTENSION.to_string(),
        json!({ "mimeTypes": [MCP_APP_MIME] }),
    );
    json!({ "extensions": extensions })
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
