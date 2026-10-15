#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Processing payload wire forms are independent of the implementation.
use super::*;
use crate::McpRemoteTool;
use serde_json::json;
#[test]
fn processing_requests_pin_enum_tags_and_optional_render_defaults() {
    let req = TransformTextRequest {
        operation: TextTransform::SanitizeForLlm,
        text: "hello".into(),
        max_bytes: 12,
    };
    assert_eq!(
        serde_json::to_value(req).unwrap(),
        json!({"operation":"sanitize_for_llm","text":"hello","max_bytes":12})
    );
    let req: RenderToolOutputRequest =
        serde_json::from_value(json!({"result":{"content":[],"is_error":false}})).unwrap();
    assert_eq!(req.format, ToolOutputFormat::Llm);
    assert!(!req.prefer_markdown);
    for format in [
        ToolOutputFormat::Text,
        ToolOutputFormat::Output,
        ToolOutputFormat::Llm,
    ] {
        assert_eq!(
            serde_json::from_value::<ToolOutputFormat>(serde_json::to_value(format).unwrap())
                .unwrap(),
            format
        );
    }
    for operation in [
        TextTransform::SanitizeForLlm,
        TextTransform::StripControlChars,
        TextTransform::StripInstructionFences,
        TextTransform::TruncateUtf8Safe,
        TextTransform::NormalizeIdentifier,
    ] {
        assert_eq!(
            serde_json::from_value::<TextTransform>(serde_json::to_value(operation).unwrap())
                .unwrap(),
            operation
        );
    }
    assert_eq!(
        serde_json::to_value(TransformTextRequest {
            operation: TextTransform::NormalizeIdentifier,
            text: "**search!**".into(),
            max_bytes: MAX_PROCESSING_BYTES,
        })
        .unwrap(),
        json!({
            "operation": "normalize_identifier",
            "text": "**search!**",
            "max_bytes": MAX_PROCESSING_BYTES,
        })
    );
    let display = RemoteToolDisplay {
        description: None,
        title: None,
    };
    assert_eq!(
        serde_json::to_value(display).unwrap(),
        json!({"description":null,"title":null})
    );

    let scrub = ScrubStaticToolsRequest {
        server: "weather".into(),
        tools: vec![McpRemoteTool {
            name: "forecast".into(),
            title: Some("Forecast".into()),
            description: Some("Read the forecast".into()),
            input_schema: json!({"type":"object"}),
            meta: None,
            annotations: None,
        }],
    };
    let wire = serde_json::to_value(&scrub).unwrap();
    assert_eq!(
        wire,
        json!({
            "server":"weather",
            "tools":[{
                "name":"forecast",
                "title":"Forecast",
                "description":"Read the forecast",
                "inputSchema":{"type":"object"}
            }]
        })
    );
    assert_eq!(
        serde_json::from_value::<ScrubStaticToolsRequest>(wire).unwrap(),
        scrub
    );
}
