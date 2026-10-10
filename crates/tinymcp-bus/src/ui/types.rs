use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `kind` a [`McpUiPresentation`] carries in a tool result's metadata.
pub const MCP_UI_KIND: &str = "mcp_ui";

/// The MIME type an MCP Apps resource declares.
pub const MCP_APP_MIME: &str = "text/html;profile=mcp-app";

/// The client capability extension that advertises MCP Apps support.
pub const MCP_APPS_EXTENSION: &str = "io.modelcontextprotocol/ui";

/// Which widget protocol a frame speaks to the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiFlavor {
    /// MCP Apps (`ui/*` JSON-RPC over `postMessage`).
    McpApps,
    /// `OpenAI` Apps SDK (`window.openai`), bridged onto the same channel.
    AppsSdk,
    /// HTML the host supplies itself; the page may not call tools.
    HostInline,
}

/// How a URL found in a tool result is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkClass {
    /// An `http(s)` page.
    Web,
    /// An app scheme (`upi://`, `phonepe://`, `intent://`, ...) meant for a
    /// phone, never opened on the host machine.
    Handoff,
    /// An `http(s)` image asset; not an action.
    Image,
    /// A scheme that could run script or reach the host, or a malformed URL.
    Blocked,
}

/// How a surfaced link may be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLinkKind {
    /// `http(s)`: opened in a browser.
    External,
    /// Any other allowed scheme: handed to a phone.
    Handoff,
}

/// A link a tool result offered, surfaced as an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiLink {
    /// The URL, as found.
    pub url: String,
    /// How it may be opened.
    pub kind: UiLinkKind,
}

/// What a host renders for one tool call that offered UI.
///
/// Carried as the tool result's metadata. Never carries the widget's HTML: a
/// host reads that separately by `resource_uri` or `inline_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpUiPresentation {
    /// Always [`MCP_UI_KIND`].
    pub kind: String,
    /// The widget protocol.
    pub flavor: UiFlavor,
    /// The server the tool belongs to; `None` for host-supplied UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    /// The tool's name on its server.
    pub tool: String,
    /// The `ui://` resource the widget loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_uri: Option<String>,
    /// A host-held document the widget loads instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_id: Option<String>,
    /// A display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The call's arguments.
    #[serde(default)]
    pub tool_input: Value,
    /// The result's `structuredContent`, when small enough to carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// The result's `_meta`, when small enough to carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_meta: Option<Value>,
    /// Links the result offered.
    #[serde(default)]
    pub links: Vec<UiLink>,
}

impl McpUiPresentation {
    /// Whether the presentation has a widget document to show.
    #[must_use]
    pub fn has_frame(&self) -> bool {
        self.resource_uri.is_some() || self.inline_id.is_some()
    }

    /// The presentation as tool-result metadata.
    #[must_use]
    pub fn to_metadata(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The `https` origins a widget may reach.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiCsp {
    /// Origins the widget may fetch from.
    #[serde(default)]
    pub connect_domains: Vec<String>,
    /// Origins the widget may load scripts, styles, images and fonts from.
    #[serde(default)]
    pub resource_domains: Vec<String>,
}

/// A widget document, validated and ready to sandbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiResource {
    /// The document.
    pub html: String,
    /// The MIME type the server declared.
    pub mime_type: String,
    /// The declared origins, reduced to `https`.
    pub csp: UiCsp,
    /// Declared permissions, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Value>,
    /// Whether the widget asks for a border.
    #[serde(default)]
    pub prefers_border: bool,
}

/// What a host does with a tool call a widget asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetCallPolicy {
    /// Call it: the tool declares it changes nothing.
    Allow,
    /// Ask the user first.
    Confirm,
    /// Refuse: the widget may not call tools, or the tool is hidden from it.
    Deny,
}

/// How a host shows one presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiRendering {
    /// Sandbox and show the widget.
    Widget,
    /// Show the classified links only.
    Links,
    /// Show nothing beyond the text.
    Nothing,
}
