//! Metadata processing requests and results; algorithms live in the module.
use crate::{McpRemoteTool, McpToolResult};
use serde::{Deserialize, Serialize};

/// Maximum serialized request or generated output bytes for metadata operations.
pub const MAX_PROCESSING_BYTES: usize = 1_048_576;

/// The lexical transformation a caller requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TextTransform {
    /// Strip controls and instruction fences, then apply the byte cap.
    SanitizeForLlm,
    /// Strip controls while preserving newline and tab.
    StripControlChars,
    /// Strip known instruction fence markers.
    StripInstructionFences,
    /// Truncate at a UTF-8 boundary with the established ellipsis suffix.
    TruncateUtf8Safe,
}

/// One bounded lexical transformation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransformTextRequest {
    /// Requested transformation.
    pub operation: TextTransform,
    /// Untrusted source text.
    pub text: String,
    /// Caller-selected cap, used by sanitization and truncation.
    pub max_bytes: usize,
}

/// Sanitized remote metadata with the MCP description and title byte caps applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteToolDisplay {
    /// Sanitized optional description.
    pub description: Option<String>,
    /// Sanitized optional title.
    pub title: Option<String>,
}

/// The projection a caller requests from a rendered tool result.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolOutputFormat {
    /// Text blocks only, joined by newlines.
    Text,
    /// Text and pretty JSON blocks, joined by newlines.
    Output,
    /// Caller-selected Markdown when nonblank, otherwise all blocks.
    #[default]
    Llm,
}

/// Bounded projection of one tool result; host presentation policy is explicit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderToolOutputRequest {
    /// Rendered tool result.
    pub result: McpToolResult,
    /// Output projection, defaulting to LLM presentation.
    #[serde(default)]
    pub format: ToolOutputFormat,
    /// Whether the caller prefers supplied Markdown for LLM presentation.
    #[serde(default)]
    pub prefer_markdown: bool,
}

/// Input to `DisplayRemoteTool` uses the ordinary shared MCP remote-tool DTO.
pub type DisplayRemoteToolRequest = McpRemoteTool;
