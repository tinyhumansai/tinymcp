//! Turning one answered tool call into its presentation.
//!
//! Pure over the call's JSON: the tool descriptor's `_meta`, the result's
//! structured content, `_meta` and embedded resources, and its text. Nothing is
//! cached or fetched here; a document the result carried is handed back for the
//! host to keep.

use base64::Engine as _;
use serde_json::Value;

use super::links::extract_links;
use super::{MCP_UI_KIND, McpUiPresentation, UiCsp, UiFlavor, UiResource};
use crate::MCP_RESULT_KIND;

/// The largest widget document a host serves to a frame.
pub const MAX_WIDGET_BYTES: usize = 2 * 1024 * 1024;
/// The largest structured content carried with a presentation.
pub const MAX_STRUCTURED_BYTES: usize = 64 * 1024;
/// The largest result `_meta` carried with a presentation.
pub const MAX_META_BYTES: usize = 16 * 1024;
/// The largest tool input carried with a presentation.
pub const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_CSP_DOMAINS: usize = 32;

/// One answered tool call, reduced to what UI resolution reads.
#[derive(Debug, Clone, Default)]
pub struct UiCallView {
    /// The server the tool belongs to.
    pub server_id: String,
    /// The tool's name on that server.
    pub tool: String,
    /// The tool descriptor's `_meta`.
    pub tool_meta: Option<Value>,
    /// The call's arguments.
    pub tool_input: Value,
    /// The result's `structuredContent`.
    pub structured_content: Option<Value>,
    /// The result's `_meta`.
    pub result_meta: Option<Value>,
    /// Embedded resources, as `{uri, mimeType, text?, blob?, _meta?}` objects.
    pub resources: Vec<Value>,
    /// The result's text, as the model saw it.
    pub text: String,
}

/// Where a widget's document comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum UiSource {
    /// A `ui://` resource on the tool's server.
    Uri {
        /// The resource URI.
        uri: String,
        /// The protocol the widget speaks.
        flavor: UiFlavor,
    },
    /// A document the result embedded.
    Embedded {
        /// The embedded resource entry.
        resource: Value,
    },
}

/// A presentation, and any widget document the result itself carried.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedUi {
    /// The presentation. `inline_id` is unset; a host that keeps
    /// [`ResolvedUi::inline_document`] assigns it.
    pub presentation: McpUiPresentation,
    /// The document for `presentation.resource_uri`, when the result embedded
    /// it, keyed by that URI.
    pub prefetched: Option<(String, UiResource)>,
    /// A widget document the result embedded without a `ui://` template.
    pub inline_document: Option<UiResource>,
}

/// Whether `uri` is a widget resource URI.
#[must_use]
pub fn is_ui_uri(uri: &str) -> bool {
    uri.len() > "ui://".len()
        && uri
            .get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("ui://"))
}

/// Whether `mime` is an HTML document type a widget may be served as.
#[must_use]
pub fn is_html_mime(mime: &str) -> bool {
    mime.trim().to_ascii_lowercase().starts_with("text/html")
}

fn meta_str<'a>(meta: Option<&'a Value>, path: &[&str]) -> Option<&'a str> {
    let mut cursor = meta?;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_str()
}

const TEMPLATE_KEYS: [(&[&str], UiFlavor); 3] = [
    (&["ui", "resourceUri"], UiFlavor::McpApps),
    (&["ui/resourceUri"], UiFlavor::McpApps),
    (&["openai/outputTemplate"], UiFlavor::AppsSdk),
];

/// The widget template a tool's `_meta` declares, by precedence:
/// `ui.resourceUri`, the legacy `ui/resourceUri`, then the Apps SDK
/// `openai/outputTemplate`. Only `ui://` URIs count.
#[must_use]
pub fn template_from_meta(meta: &Value) -> Option<(String, UiFlavor)> {
    TEMPLATE_KEYS.iter().find_map(|(path, flavor)| {
        meta_str(Some(meta), path)
            .map(str::trim)
            .filter(|uri| is_ui_uri(uri))
            .map(|uri| (uri.to_string(), *flavor))
    })
}

/// The source a call's widget loads from: a template declared on the tool or
/// the result (tool first, at each precedence level), then an embedded `ui://`
/// HTML resource.
#[must_use]
pub fn select_source(view: &UiCallView) -> Option<UiSource> {
    let metas = [view.tool_meta.as_ref(), view.result_meta.as_ref()];
    for (path, flavor) in TEMPLATE_KEYS {
        for meta in metas {
            if let Some(uri) = meta_str(meta, path).map(str::trim) {
                if is_ui_uri(uri) {
                    return Some(UiSource::Uri {
                        uri: uri.to_string(),
                        flavor,
                    });
                }
                tracing::debug!(tool = %view.tool, "[mcp_ui] ignored a non-ui:// resource URI");
            }
        }
    }
    view.resources
        .iter()
        .find(|resource| {
            let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("");
            let mime = resource
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("");
            is_ui_uri(uri) && is_html_mime(mime)
        })
        .map(|resource| UiSource::Embedded {
            resource: resource.clone(),
        })
}

/// `value` when it is non-null and serializes to at most `limit` bytes.
#[must_use]
pub fn bounded(value: Option<&Value>, limit: usize) -> Option<Value> {
    let value = value.filter(|value| !value.is_null())?;
    let size = serde_json::to_vec(value).map(|bytes| bytes.len()).ok()?;
    if size > limit {
        tracing::debug!(size, limit, "[mcp_ui] dropped an oversized field");
        return None;
    }
    Some(value.clone())
}

/// The presentation for one call, or `None` when it offered neither a widget
/// nor a link.
#[must_use]
pub fn resolve_presentation(view: &UiCallView) -> Option<ResolvedUi> {
    let links = extract_links(view.structured_content.as_ref(), &view.text);
    let mut prefetched = None;
    let mut inline_document = None;
    let (resource_uri, flavor) = match select_source(view) {
        Some(UiSource::Uri { uri, flavor }) => {
            prefetched = view
                .resources
                .iter()
                .find(|resource| resource.get("uri").and_then(Value::as_str) == Some(uri.as_str()))
                .and_then(|embedded| {
                    resource_from_contents(std::slice::from_ref(embedded), &uri).ok()
                })
                .map(|document| (uri.clone(), document));
            (Some(uri), flavor)
        }
        Some(UiSource::Embedded { resource }) => {
            let uri = resource
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match resource_from_contents(std::slice::from_ref(&resource), &uri) {
                Ok(document) => inline_document = Some(document),
                Err(error) => {
                    tracing::debug!(tool = %view.tool, %error, "[mcp_ui] embedded widget rejected");
                }
            }
            (None, UiFlavor::McpApps)
        }
        None => (None, UiFlavor::McpApps),
    };
    if resource_uri.is_none() && inline_document.is_none() && links.is_empty() {
        return None;
    }
    tracing::debug!(
        server_id = %view.server_id,
        tool = %view.tool,
        frame = resource_uri.is_some() || inline_document.is_some(),
        links = links.len(),
        "[mcp_ui] tool call offered UI"
    );
    Some(ResolvedUi {
        presentation: McpUiPresentation {
            kind: MCP_UI_KIND.to_string(),
            flavor,
            server_id: Some(view.server_id.clone()),
            tool: view.tool.clone(),
            resource_uri,
            inline_id: None,
            title: None,
            tool_input: bounded(Some(&view.tool_input), MAX_INPUT_BYTES).unwrap_or(Value::Null),
            structured_content: bounded(view.structured_content.as_ref(), MAX_STRUCTURED_BYTES),
            result_meta: bounded(view.result_meta.as_ref(), MAX_META_BYTES),
            links,
        },
        prefetched,
        inline_document,
    })
}

fn normalize_resource_entry(entry: &Value) -> Value {
    match entry.get("type").and_then(Value::as_str) {
        Some("resource") => entry.get("resource").cloned().unwrap_or(Value::Null),
        _ => entry.clone(),
    }
}

fn resources_from_content(content: Option<&Value>) -> Vec<Value> {
    content
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("resource"))
                .map(normalize_resource_entry)
                .collect()
        })
        .unwrap_or_default()
}

fn text_from_content(content: Option<&Value>) -> String {
    content
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// A view over a server's raw `tools/call` reply.
#[must_use]
pub fn view_from_raw_result(
    server_id: &str,
    tool: &str,
    tool_meta: Option<Value>,
    tool_input: Value,
    raw: &Value,
) -> UiCallView {
    UiCallView {
        server_id: server_id.to_string(),
        tool: tool.to_string(),
        tool_meta,
        tool_input,
        structured_content: raw.get("structuredContent").cloned(),
        result_meta: raw.get("_meta").cloned(),
        resources: resources_from_content(raw.get("content")),
        text: text_from_content(raw.get("content")),
    }
}

/// A view over the `mcp_result` envelope a tool result's metadata carries.
///
/// `None` when `metadata` is not that envelope.
#[must_use]
pub fn view_from_envelope(
    metadata: &Value,
    tool_meta: Option<Value>,
    tool_input: Value,
    text: &str,
) -> Option<UiCallView> {
    if metadata.get("kind").and_then(Value::as_str) != Some(MCP_RESULT_KIND) {
        return None;
    }
    let server_id = metadata.get("server").and_then(Value::as_str)?;
    let tool = metadata.get("tool").and_then(Value::as_str)?;
    let resources = metadata
        .get("resources")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().map(normalize_resource_entry).collect())
        .unwrap_or_default();
    Some(UiCallView {
        server_id: server_id.to_string(),
        tool: tool.to_string(),
        tool_meta,
        tool_input,
        structured_content: metadata
            .get("structured_content")
            .filter(|value| !value.is_null())
            .cloned(),
        result_meta: metadata
            .get("meta")
            .filter(|value| !value.is_null())
            .cloned(),
        resources,
        text: text.to_string(),
    })
}

/// The widget document for `uri` among a `resources/read` reply's contents.
///
/// # Errors
///
/// When no entry matches, the entry is not HTML, has no text, or is larger
/// than [`MAX_WIDGET_BYTES`].
pub fn resource_from_contents(contents: &[Value], uri: &str) -> Result<UiResource, String> {
    let entry = contents
        .iter()
        .find(|entry| entry.get("uri").and_then(Value::as_str) == Some(uri))
        .or_else(|| (contents.len() == 1).then(|| &contents[0]))
        .ok_or_else(|| "the server returned no document for this URI".to_string())?;
    let mime_type = entry
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !is_html_mime(&mime_type) {
        return Err(format!("unsupported widget type `{mime_type}`"));
    }
    let html = match (
        entry.get("text").and_then(Value::as_str),
        entry.get("blob").and_then(Value::as_str),
    ) {
        (Some(text), _) => {
            if text.len() > MAX_WIDGET_BYTES {
                return Err("the widget document is too large".to_string());
            }
            text.to_string()
        }
        (None, Some(blob)) => {
            if blob.len() > MAX_WIDGET_BYTES * 4 / 3 + 4 {
                return Err("the widget document is too large".to_string());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(blob.trim())
                .map_err(|_| "the widget document is not valid base64".to_string())?;
            String::from_utf8(bytes).map_err(|_| "the widget document is not UTF-8".to_string())?
        }
        (None, None) => return Err("the widget document is empty".to_string()),
    };
    if html.len() > MAX_WIDGET_BYTES {
        return Err("the widget document is too large".to_string());
    }
    let meta = entry.get("_meta");
    Ok(UiResource {
        html,
        mime_type,
        csp: csp_from_meta(meta),
        permissions: meta
            .and_then(|meta| meta.pointer("/ui/permissions"))
            .filter(|value| value.is_object())
            .cloned(),
        prefers_border: meta
            .and_then(|meta| {
                meta.pointer("/ui/prefersBorder")
                    .or_else(|| meta.get("openai/widgetPrefersBorder"))
            })
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn csp_from_meta(meta: Option<&Value>) -> UiCsp {
    let Some(meta) = meta else {
        return UiCsp::default();
    };
    let (connect, resource) = if let Some(csp) = meta.pointer("/ui/csp") {
        (csp.get("connectDomains"), csp.get("resourceDomains"))
    } else if let Some(csp) = meta.get("openai/widgetCSP") {
        (csp.get("connect_domains"), csp.get("resource_domains"))
    } else {
        (None, None)
    };
    UiCsp {
        connect_domains: https_origins(connect),
        resource_domains: https_origins(resource),
    }
}

/// Each declared origin that is `https`, reduced to `https://host[:port]`;
/// a leading `*.` wildcard label is kept.
#[must_use]
pub fn https_origins(value: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(items) = value.and_then(Value::as_array) else {
        return out;
    };
    for item in items.iter().filter_map(Value::as_str) {
        if out.len() >= MAX_CSP_DOMAINS {
            break;
        }
        if let Some(origin) = https_origin(item)
            && !out.contains(&origin)
        {
            out.push(origin);
        }
    }
    out
}

fn https_origin(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let rest = raw
        .get(..8)
        .filter(|prefix| prefix.eq_ignore_ascii_case("https://"))
        .map(|_| &raw[8..])?;
    let authority = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host.to_string(), Some(port.to_string()))
        }
        Some(_) => return None,
        None => (authority.clone(), None),
    };
    let bare = host.strip_prefix("*.").unwrap_or(&host);
    let labels_ok = !bare.is_empty()
        && bare.contains('.')
        && bare.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if !labels_ok {
        return None;
    }
    Some(match port {
        Some(port) => format!("https://{host}:{port}"),
        None => format!("https://{host}"),
    })
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
