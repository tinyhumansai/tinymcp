#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Processing payload wire forms are independent of the implementation.
use super::*;
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
    ] {
        assert_eq!(
            serde_json::from_value::<TextTransform>(serde_json::to_value(operation).unwrap())
                .unwrap(),
            operation
        );
    }
    let display = RemoteToolDisplay {
        description: None,
        title: None,
    };
    assert_eq!(
        serde_json::to_value(display).unwrap(),
        json!({"description":null,"title":null})
    );
}
