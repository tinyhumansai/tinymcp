//! The vocabulary for tool-provided UI.
//!
//! A server can attach a widget to a tool (MCP Apps `_meta.ui.resourceUri`, or
//! the Apps SDK `openai/outputTemplate`) and put links in its results. A host
//! reduces one answered call to a [`McpUiPresentation`]: which widget, which
//! data, which links. These are the shapes that presentation is made of, so
//! every host that renders tool UI reads and writes the same metadata.
//!
//! The rules that produce a presentation (template resolution, link
//! classification, size caps, the widget tool-call policy) live in
//! `tinymcp::ui`.

mod types;

pub use types::{
    LinkClass, MCP_APP_MIME, MCP_APPS_EXTENSION, MCP_UI_KIND, McpUiPresentation, UiCsp, UiFlavor,
    UiLink, UiLinkKind, UiRendering, UiResource, WidgetCallPolicy,
};

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
