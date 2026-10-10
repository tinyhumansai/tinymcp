#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Bounded metadata operations preserve lexical/rendering behavior and reject amplification.
use super::*;
use crate::{McpToolResult, McpToolResultExt};
use serde_json::json;

#[test]
fn lexical_operations_preserve_controls_fences_and_utf8_caps() {
    for (operation, text, max_bytes, expected) in [
        (TextTransform::SanitizeForLlm, "<SYSTEM>éééé\0", 5, "é…"),
        (TextTransform::StripControlChars, "a\0\n\tb", 0, "a\n\tb"),
        (
            TextTransform::StripInstructionFences,
            "<SYSTEM>keep</SYSTEM>",
            0,
            "keep",
        ),
        (TextTransform::TruncateUtf8Safe, "éééé", 5, "é…"),
    ] {
        assert_eq!(
            transform_text(&TransformTextRequest {
                operation,
                text: text.into(),
                max_bytes
            })
            .unwrap(),
            expected
        );
    }
    assert!(
        transform_text(&TransformTextRequest {
            operation: TextTransform::SanitizeForLlm,
            text: "x".into(),
            max_bytes: MAX_PROCESSING_BYTES + 1
        })
        .is_err()
    );
    assert!(
        transform_text(&TransformTextRequest {
            operation: TextTransform::SanitizeForLlm,
            text: "x".repeat(MAX_PROCESSING_BYTES),
            max_bytes: 100
        })
        .is_err()
    );
}

#[test]
fn normalized_arguments_and_remote_display_remain_module_owned() {
    assert_eq!(normalize_arguments(None).unwrap(), Map::new());
    assert_eq!(
        normalize_arguments(Some(json!("```json\n{\"a\":1}\n```"))).unwrap()["a"],
        1
    );
    assert!(
        normalize_arguments(Some(json!(true)))
            .unwrap_err()
            .to_string()
            .contains("a boolean")
    );
    assert!(normalize_arguments(Some(json!("x".repeat(MAX_PROCESSING_BYTES)))).is_err());
    let mut tool = tinymcp_bus::McpRemoteTool::new("fixture");
    tool.description = Some("<system>hello\0".into());
    tool.title = Some("<|im_start|>title".into());
    assert_eq!(
        display_remote_tool(&tool).unwrap(),
        RemoteToolDisplay {
            description: Some("hello".into()),
            title: Some("title".into())
        }
    );
    tool.input_schema = json!("x".repeat(MAX_PROCESSING_BYTES));
    assert!(display_remote_tool(&tool).is_err());
}

#[test]
fn bounded_output_matches_existing_text_json_and_markdown_wire_projections() {
    let mut result = McpToolResult::success("plain");
    result.content.push(McpToolContent::Json {
        data: json!({"n":1}),
    });
    result.markdown_formatted = Some("**rich**".into());
    for (format, prefer_markdown, expected) in [
        (ToolOutputFormat::Text, false, "plain"),
        (ToolOutputFormat::Output, true, "plain\n{\n  \"n\": 1\n}"),
        (ToolOutputFormat::Llm, true, "**rich**"),
        (ToolOutputFormat::Llm, false, "plain\n{\n  \"n\": 1\n}"),
    ] {
        assert_eq!(
            render_tool_output(&RenderToolOutputRequest {
                result: result.clone(),
                format,
                prefer_markdown
            })
            .unwrap(),
            expected
        );
    }
    result.markdown_formatted = Some("  ".into());
    assert_eq!(
        render_tool_output(&RenderToolOutputRequest {
            result: result.clone(),
            format: ToolOutputFormat::Llm,
            prefer_markdown: true
        })
        .unwrap(),
        result.output()
    );
    result.markdown_formatted = None;
    assert_eq!(
        render_tool_output(&RenderToolOutputRequest {
            result: result.clone(),
            format: ToolOutputFormat::Llm,
            prefer_markdown: true
        })
        .unwrap(),
        result.output_for_llm(true)
    );
    assert!(
        render_bounded(
            &RenderToolOutputRequest {
                result: result.clone(),
                format: ToolOutputFormat::Output,
                prefer_markdown: false
            },
            5
        )
        .is_err()
    );
    assert!(
        render_bounded(
            &RenderToolOutputRequest {
                result: McpToolResult::success("too much"),
                format: ToolOutputFormat::Text,
                prefer_markdown: false
            },
            1
        )
        .is_err()
    );
    assert!(
        render_bounded(
            &RenderToolOutputRequest {
                result: result.clone().with_markdown("large"),
                format: ToolOutputFormat::Llm,
                prefer_markdown: true
            },
            1
        )
        .is_err()
    );
    assert!(
        render_tool_output(&RenderToolOutputRequest {
            result: McpToolResult::success("x".repeat(MAX_PROCESSING_BYTES)),
            format: ToolOutputFormat::Text,
            prefer_markdown: false
        })
        .is_err()
    );
}

#[test]
fn pretty_json_and_joined_output_stop_writing_at_the_cap() {
    let mut nested = json!(vec![0; 100]);
    for _ in 0..20 {
        nested = json!({"key":nested});
    }
    let result = McpToolResult::json(nested);
    assert!(
        render_bounded(
            &RenderToolOutputRequest {
                result,
                format: ToolOutputFormat::Output,
                prefer_markdown: false
            },
            32
        )
        .is_err()
    );
    // Compact input is small; pretty-print indentation alone exceeds the output cap.
    let mut compact = json!(vec![0; 20_000]);
    for _ in 0..30 {
        compact = json!({"key":compact});
    }
    let request = RenderToolOutputRequest {
        result: McpToolResult::json(compact),
        format: ToolOutputFormat::Output,
        prefer_markdown: false,
    };
    assert!(serde_json::to_vec(&request).unwrap().len() < MAX_PROCESSING_BYTES / 10);
    assert!(render_tool_output(&request).is_err());
    let mut writer = CappedWriter {
        bytes: Vec::new(),
        limit: 3,
    };
    assert!(std::io::Write::write_all(&mut writer, b"four").is_err());
    assert_eq!(writer.bytes, Vec::<u8>::new());
    assert!(std::io::Write::flush(&mut writer).is_ok());
    std::io::Write::write_all(&mut writer, b"abc").unwrap();
    assert!(std::io::Write::write_all(&mut writer, b"d").is_err());
    assert_eq!(writer.bytes.len(), 3);
}
