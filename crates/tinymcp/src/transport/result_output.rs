//! Implementation extensions for shared wire vocabulary.

use tinymcp_bus::{McpToolContent, McpToolResult};
/// Behavior implemented by the module rather than the contract crate.
pub trait McpToolResultExt: Sized {
    /// The text blocks, joined by newlines. JSON blocks are skipped.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{McpToolResult, McpToolResultExt};
    /// assert!(McpToolResult::json(serde_json::json!({"k": 1})).text().is_empty());
    /// ```
    #[must_use]
    fn text(&self) -> String;

    /// Every block rendered to text, joined by newlines.
    ///
    /// Unlike [`Self::text`], JSON blocks are pretty-printed rather than
    /// skipped. A block that cannot be serialized contributes an empty string
    /// rather than failing the whole rendering — one malformed block should not
    /// cost a caller the rest of the result.
    #[must_use]
    fn output(&self) -> String;

    /// The Markdown rendering when present and non-blank, else [`Self::output`].
    ///
    /// `prefer_markdown` is the caller's policy, not a property of the result,
    /// so it is passed rather than stored.
    #[must_use]
    fn output_for_llm(&self, prefer_markdown: bool) -> String;
}

impl McpToolResultExt for McpToolResult {
    /// The text blocks, joined by newlines. JSON blocks are skipped.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{McpToolResult, McpToolResultExt};
    /// assert!(McpToolResult::json(serde_json::json!({"k": 1})).text().is_empty());
    /// ```
    fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                McpToolContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every block rendered to text, joined by newlines.
    ///
    /// Unlike [`Self::text`], JSON blocks are pretty-printed rather than
    /// skipped. A block that cannot be serialized contributes an empty string
    /// rather than failing the whole rendering — one malformed block should not
    /// cost a caller the rest of the result.
    fn output(&self) -> String {
        self.content
            .iter()
            .map(|block| match block {
                McpToolContent::Text { text } => text.clone(),
                McpToolContent::Json { data } => {
                    serde_json::to_string_pretty(data).unwrap_or_default()
                }
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The Markdown rendering when present and non-blank, else [`Self::output`].
    ///
    /// `prefer_markdown` is the caller's policy, not a property of the result,
    /// so it is passed rather than stored.
    fn output_for_llm(&self, prefer_markdown: bool) -> String {
        if prefer_markdown
            && let Some(markdown) = self.markdown_formatted.as_deref()
            && !markdown.trim().is_empty()
        {
            return markdown.to_string();
        }
        self.output()
    }
}
