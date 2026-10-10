//! Tool-provided UI for hosts: the rules that turn one answered MCP tool call
//! into what a host shows for it.
//!
//! - [`resolve_presentation`] reads the tool's `_meta` template
//!   (`ui.resourceUri`, or the Apps SDK `openai/outputTemplate`), the result's
//!   structured content and embedded resources, and its text, and returns a
//!   [`McpUiPresentation`] with size-capped data and classified links.
//! - [`resource_from_contents`] validates a widget document: HTML only, at
//!   most [`MAX_WIDGET_BYTES`], CSP origins reduced to `https`.
//! - [`classify_link`] sorts a URL into web, phone handoff, image, or blocked;
//!   [`extract_links`] surfaces only the first two.
//! - [`widget_call_policy`] decides whether a widget's tool call runs, waits
//!   for the user, or is refused; [`rendering`] decides whether a host shows
//!   the widget, the links, or nothing.
//!
//! A host that cannot sandbox HTML (a messaging or email channel) does not
//! advertise [`client_capabilities`], so servers that follow the MCP Apps spec
//! answer with text, and renders [`UiRendering::Links`] from the classified
//! links. The vocabulary lives in `tinymcp_bus::ui`.

mod links;
mod policy;
mod resolve;

pub use links::{MAX_LINKS, classify_link, extract_links, is_image_url, link_kind};
pub use policy::{
    client_capabilities, is_read_only, rendering, visible_to_app, widget_call_policy,
};
pub use resolve::{
    MAX_INPUT_BYTES, MAX_META_BYTES, MAX_STRUCTURED_BYTES, MAX_WIDGET_BYTES, ResolvedUi,
    UiCallView, UiSource, bounded, https_origins, is_html_mime, is_ui_uri, resolve_presentation,
    resource_from_contents, select_source, template_from_meta, view_from_envelope,
    view_from_raw_result,
};
pub use tinymcp_bus::ui::{
    LinkClass, MCP_APP_MIME, MCP_APPS_EXTENSION, MCP_UI_KIND, McpUiPresentation, UiCsp, UiFlavor,
    UiLink, UiLinkKind, UiRendering, UiResource, WidgetCallPolicy,
};
