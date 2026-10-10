//! Wire-shape tests for the tool UI vocabulary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::json;

#[test]
fn presentation_wire_shape_is_stable() {
    let presentation = McpUiPresentation {
        kind: MCP_UI_KIND.to_string(),
        flavor: UiFlavor::AppsSdk,
        server_id: Some("srv".into()),
        tool: "search".into(),
        resource_uri: Some("ui://widget/search.html".into()),
        inline_id: None,
        title: None,
        tool_input: json!({"q": "bars"}),
        structured_content: Some(json!({"items": []})),
        result_meta: None,
        links: vec![UiLink {
            url: "upi://pay?pa=a@b".into(),
            kind: UiLinkKind::Handoff,
        }],
    };
    assert_eq!(
        presentation.to_metadata(),
        json!({
            "kind": "mcp_ui",
            "flavor": "apps_sdk",
            "server_id": "srv",
            "tool": "search",
            "resource_uri": "ui://widget/search.html",
            "tool_input": {"q": "bars"},
            "structured_content": {"items": []},
            "links": [{"url": "upi://pay?pa=a@b", "kind": "handoff"}]
        })
    );
    assert!(presentation.has_frame());
}

#[test]
fn minimal_presentation_decodes_with_defaults() {
    let presentation: McpUiPresentation = serde_json::from_value(
        json!({"kind": "mcp_ui", "flavor": "host_inline", "tool": "show_ui"}),
    )
    .expect("decodes");
    assert!(!presentation.has_frame());
    assert_eq!(presentation.links, Vec::new());
    assert_eq!(presentation.tool_input, serde_json::Value::Null);
}

#[test]
fn enums_use_snake_case() {
    assert_eq!(json!(LinkClass::Image), json!("image"));
    assert_eq!(json!(UiLinkKind::External), json!("external"));
    assert_eq!(json!(WidgetCallPolicy::Confirm), json!("confirm"));
    assert_eq!(json!(UiRendering::Links), json!("links"));
    assert_eq!(json!(UiFlavor::McpApps), json!("mcp_apps"));
}
