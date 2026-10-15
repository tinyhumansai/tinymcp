//! Bounded module-owned metadata preparation over shared contract requests.
use crate::{Error, McpRemoteToolExt, Result};
use serde_json::{Map, Value, json};
use tinymcp_bus::{
    DisplayRemoteToolRequest, MAX_PROCESSING_BYTES, McpToolContent, McpToolPresentation,
    McpToolResult, PrepareAgentToolsRequest, RemoteToolDisplay, RenderToolOutputRequest,
    ScrubStaticToolsRequest, TextTransform, ToolOutputFormat, TransformTextRequest,
};

const MAX_AGENT_TOOL_SOURCES: usize = 256;
const MAX_AGENT_TOOL_PRESENTATIONS: usize = 4096;

/// The largest model-facing MCP result or schema block before it is elided.
pub const MAX_LLM_BLOCK_BYTES: usize = 64 * 1024;

/// Applies the model-facing result caps while preserving host-only reply data.
pub(crate) fn bound_mcp_tool_result(mut result: McpToolResult) -> McpToolResult {
    result.content = result
        .content
        .into_iter()
        .map(|block| match block {
            McpToolContent::Text { text } => McpToolContent::Text {
                text: bound_text(text),
            },
            McpToolContent::Json { data } => McpToolContent::Json {
                data: bound_json(data),
            },
            other => {
                let serialized = serde_json::to_vec(&other);
                bound_other_content(other, serialized)
            }
        })
        .collect();
    result.markdown_formatted = result.markdown_formatted.map(bound_text);
    result
}

fn bound_other_content(
    block: McpToolContent,
    serialized: std::result::Result<Vec<u8>, serde_json::Error>,
) -> McpToolContent {
    match serialized {
        Ok(bytes) if bytes.len() <= MAX_LLM_BLOCK_BYTES => block,
        Ok(_) => McpToolContent::Json {
            data: elide_oversized_block(&block),
        },
        Err(_) => McpToolContent::Json {
            data: json!({
                "type": "unknown",
                "data": "[content elided: serialization failed]"
            }),
        },
    }
}

pub(crate) fn bound_text(text: String) -> String {
    if text.len() <= MAX_LLM_BLOCK_BYTES {
        text
    } else {
        elision_marker(text.len())
    }
}

pub(crate) fn bound_json(data: Value) -> Value {
    let serialized = serde_json::to_string(&data).unwrap_or_default();
    if serialized.len() <= MAX_LLM_BLOCK_BYTES {
        data
    } else {
        Value::String(elision_marker(serialized.len()))
    }
}

pub(crate) fn elide_oversized_block(block: &McpToolContent) -> Value {
    let value = serde_json::to_value(block).unwrap_or(Value::Null);
    let serialized = serde_json::to_string(&value).unwrap_or_default();
    if serialized.len() <= MAX_LLM_BLOCK_BYTES {
        return value;
    }
    let kind = value.get("type").cloned().unwrap_or(Value::Null);
    json!({"type": kind, "data": elision_marker(serialized.len())})
}

fn elision_marker(bytes: usize) -> String {
    format!("[{bytes} bytes elided]")
}

fn limit_error() -> Error {
    Error::invalid_argument("metadata processing exceeds byte limit")
}

struct CappedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl std::io::Write for CappedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other(
                "metadata processing exceeds byte limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn bounded<T: serde::Serialize>(value: &T) -> Result<()> {
    serde_json::to_writer(
        &mut CappedWriter {
            bytes: Vec::new(),
            limit: MAX_PROCESSING_BYTES,
        },
        value,
    )
    .map_err(|_| limit_error())
}

/// Applies shared lexical sanitization with bounded request/output sizes.
/// # Errors
/// Rejects requests or caps over the module processing byte limit.
pub fn transform_text(request: &TransformTextRequest) -> Result<String> {
    bounded(request)?;
    if request.max_bytes > MAX_PROCESSING_BYTES {
        return Err(limit_error());
    }
    let text = match request.operation {
        TextTransform::SanitizeForLlm => {
            crate::sanitize::sanitize_for_llm(&request.text, request.max_bytes)
        }
        TextTransform::StripControlChars => crate::sanitize::strip_control_chars(&request.text),
        TextTransform::StripInstructionFences => {
            crate::sanitize::strip_instruction_fences(&request.text)
        }
        TextTransform::TruncateUtf8Safe => {
            crate::sanitize::truncate_utf8_safe(&request.text, request.max_bytes)
        }
        TextTransform::NormalizeIdentifier => normalize_identifier_text(&request.text)
            .ok_or_else(|| Error::invalid_argument("missing identifier"))?,
        _ => {
            return Err(Error::invalid_argument(
                "unsupported metadata transformation",
            ));
        }
    };
    Ok(text)
}

/// Normalizes an identifier that a model may have wrapped in markdown.
/// Punctuation is removed only when it follows a markdown wrapper.
pub(crate) fn normalize_identifier_text(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let unpunctuated = value.trim_end_matches(['.', ',', ';', ':', '!']);
    let fenced = if unpunctuated.ends_with(['`', '*']) {
        unpunctuated
    } else {
        value
    };
    let cleaned = fenced.trim_matches(['`', '*']);
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

/// Reads tolerant model arguments in the implementation, without host copies.
/// # Errors
/// Rejects oversized arguments and preserves the normalizer's precise refusal detail.
pub fn normalize_arguments(arguments: Option<Value>) -> Result<Map<String, Value>> {
    bounded(&arguments)?;
    crate::normalize_tool_arguments(arguments)
        .map_err(|reason| Error::invalid_argument(reason.to_string()))
}

/// Prepares remote tool metadata with the established MCP presentation caps.
/// # Errors
/// Rejects oversized serialized tool declarations.
pub fn display_remote_tool(tool: &DisplayRemoteToolRequest) -> Result<RemoteToolDisplay> {
    bounded(tool)?;
    Ok(RemoteToolDisplay {
        description: tool.display_description(),
        title: tool.display_title(),
    })
}

/// Removes this configured server's credentials from remote tool metadata.
/// # Errors
/// Rejects an oversized serialized request.
pub fn scrub_static_tools(
    request: &ScrubStaticToolsRequest,
    scrubber: &crate::agent_tools::SecretScrubber,
) -> Result<Vec<tinymcp_bus::McpRemoteTool>> {
    bounded(request)?;
    let mut tools = request.tools.clone();
    for tool in &mut tools {
        if let Some(title) = &mut tool.title {
            *title = scrubber.scrub(title);
        }
        if let Some(description) = &mut tool.description {
            *description = scrubber.scrub(description);
        }
        scrubber.scrub_value(&mut tool.input_schema);
        if let Some(meta) = &mut tool.meta {
            scrubber.scrub_value(meta);
        }
        if let Some(annotations) = &mut tool.annotations {
            scrubber.scrub_value(annotations);
        }
    }
    Ok(tools)
}

/// Builds the stable model-facing declarations for already-screened sources.
/// # Errors
/// Rejects oversized requests, source counts, or generated declarations.
pub fn prepare_agent_tools(request: &PrepareAgentToolsRequest) -> Result<Vec<McpToolPresentation>> {
    bounded(request)?;
    if request.sources.len() > MAX_AGENT_TOOL_SOURCES {
        return Err(limit_error());
    }
    let tools = crate::agent_tools::prepare_agent_tools(&request.sources);
    if tools.len() > MAX_AGENT_TOOL_PRESENTATIONS {
        return Err(limit_error());
    }
    bounded(&tools)?;
    Ok(tools)
}

/// Projects a tool result without allocating oversized pretty JSON or joined output.
/// # Errors
/// Rejects oversized requests or projections while writing, before further buffering.
pub fn render_tool_output(request: &RenderToolOutputRequest) -> Result<String> {
    bounded(request)?;
    render_bounded(request, MAX_PROCESSING_BYTES)
}
fn render_bounded(request: &RenderToolOutputRequest, limit: usize) -> Result<String> {
    use std::io::Write;
    let mut writer = CappedWriter {
        bytes: Vec::new(),
        limit,
    };
    if request.format == ToolOutputFormat::Llm
        && request.prefer_markdown
        && let Some(markdown) = request.result.markdown_formatted.as_deref()
        && !markdown.trim().is_empty()
    {
        writer
            .write_all(markdown.as_bytes())
            .map_err(|_| limit_error())?;
    } else {
        let mut count = 0;
        for block in &request.result.content {
            if request.format == ToolOutputFormat::Text
                && !matches!(block, McpToolContent::Text { .. })
            {
                continue;
            }
            if count > 0 {
                writer.write_all(b"\n").map_err(|_| limit_error())?;
            }
            match block {
                McpToolContent::Text { text } => writer
                    .write_all(text.as_bytes())
                    .map_err(|_| limit_error())?,
                McpToolContent::Json { data } => {
                    serde_json::to_writer_pretty(&mut writer, data).map_err(|_| limit_error())?;
                }
                _ => {}
            }
            count += 1;
        }
    }
    String::from_utf8(writer.bytes)
        .map_err(|_| Error::invalid_argument("invalid metadata output encoding"))
}
#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
