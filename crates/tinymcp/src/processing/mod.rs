//! Bounded module-owned metadata preparation over shared contract requests.
use crate::{Error, McpRemoteToolExt, Result};
use serde_json::{Map, Value};
use tinymcp_bus::{
    DisplayRemoteToolRequest, MAX_PROCESSING_BYTES, McpToolContent, RemoteToolDisplay,
    RenderToolOutputRequest, TextTransform, ToolOutputFormat, TransformTextRequest,
};

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
        _ => {
            return Err(Error::invalid_argument(
                "unsupported metadata transformation",
            ));
        }
    };
    Ok(text)
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
