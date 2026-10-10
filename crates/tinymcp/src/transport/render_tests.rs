//! Unit tests for rendering a tool reply's host-facing parts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use tinymcp_bus::MAX_RESOURCE_BYTES;

use super::render_tool_result;

/// The same reply with every host-facing part stripped.
fn without_host_parts(reply: &Value) -> Value {
    let mut stripped = reply.clone();
    let object = stripped.as_object_mut().unwrap();
    object.remove("structuredContent");
    object.remove("_meta");
    stripped
}

#[test]
fn structured_content_meta_and_resources_are_kept() {
    let reply = crate::transport::ui_fixture::call_reply();

    let rendered = render_tool_result(&reply);

    assert_eq!(
        rendered.structured_content,
        Some(json!({ "temperature": 21 }))
    );
    assert_eq!(rendered.meta, reply.get("_meta").cloned());
    assert_eq!(rendered.resources.len(), 1);
    assert_eq!(rendered.resources[0].uri, "ui://card");
}

#[test]
fn the_rendered_text_is_byte_identical_with_or_without_host_parts() {
    let reply = crate::transport::ui_fixture::call_reply();

    let rich = render_tool_result(&reply);
    let plain = render_tool_result(&without_host_parts(&reply));

    assert_eq!(rich.content, plain.content);
    assert_eq!(rich.text(), plain.text());
    assert_eq!(rich.output(), plain.output());
    assert_eq!(rich.output_for_llm(true), plain.output_for_llm(true));
    assert_eq!(rich.text(), crate::transport::ui_fixture::CALL_TEXT);
}

#[test]
fn a_reply_without_host_parts_leaves_them_empty() {
    let rendered = render_tool_result(&json!({
        "content": [{ "type": "text", "text": "hi" }],
        "structuredContent": null,
    }));

    assert_eq!(rendered.structured_content, None);
    assert_eq!(rendered.meta, None);
    assert_eq!(rendered.resources.len(), 0);
    assert_eq!(
        serde_json::to_value(&rendered).unwrap(),
        json!({ "content": [{ "type": "text", "text": "hi" }], "is_error": false })
    );
}

#[test]
fn an_oversized_or_undecodable_embedded_resource_is_dropped() {
    let rendered = render_tool_result(&json!({
        "content": [
            { "type": "text", "text": "hi" },
            { "type": "resource", "resource": { "uri": "ui://big", "text": "a".repeat(MAX_RESOURCE_BYTES + 1) } },
            { "type": "resource", "resource": { "text": "no uri" } },
            { "type": "resource" },
            { "type": "resource", "resource": { "uri": "ui://ok", "blob": "AA==" } },
        ],
    }));

    assert_eq!(rendered.text(), "hi");
    assert_eq!(rendered.resources.len(), 1);
    assert_eq!(rendered.resources[0].uri, "ui://ok");
}

#[test]
fn an_error_reply_keeps_its_structured_content() {
    let rendered = render_tool_result(&json!({
        "content": [{ "type": "text", "text": "nope" }],
        "isError": true,
        "structuredContent": { "code": 7 },
    }));

    assert!(rendered.is_error);
    assert_eq!(rendered.structured_content, Some(json!({ "code": 7 })));
}
