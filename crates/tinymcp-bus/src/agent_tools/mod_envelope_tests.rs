//! Unit tests for the `mcp_result` envelope and for reading a call outcome out
//! of either metadata shape.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::{
    MCP_CALL_RESULT_KIND, MCP_RESULT_KIND, McpCallError, McpCallOutcome, McpResultEnvelope,
};
use crate::McpToolResult;

fn rich_result() -> McpToolResult {
    let mut result = McpToolResult::success("ok");
    result.structured_content = Some(json!({ "n": 1 }));
    result.meta = Some(json!({ "ui": { "resourceUri": "ui://card" } }));
    result.resources =
        vec![serde_json::from_value(json!({ "uri": "ui://card", "text": "<p>" })).unwrap()];
    result
}

#[test]
fn the_envelope_wire_form_is_pinned() {
    let envelope = McpResultEnvelope::new("docs", "search", &rich_result())
        .with_outcome(McpCallOutcome::answered("docs", "search"));

    assert_eq!(
        envelope.to_metadata(),
        json!({
            "kind": MCP_RESULT_KIND,
            "server": "docs",
            "tool": "search",
            "structured_content": { "n": 1 },
            "meta": { "ui": { "resourceUri": "ui://card" } },
            "resources": [{ "uri": "ui://card", "text": "<p>" }],
            "outcome": {
                "kind": MCP_CALL_RESULT_KIND,
                "server": "docs",
                "tool": "search",
                "ok": true,
            },
        })
    );
}

#[test]
fn an_empty_envelope_omits_every_optional_member() {
    let envelope = McpResultEnvelope::new("docs", "search", &McpToolResult::success("ok"));

    assert_eq!(
        envelope.to_metadata(),
        json!({ "kind": MCP_RESULT_KIND, "server": "docs", "tool": "search" })
    );
}

#[test]
fn the_envelope_round_trips() {
    let envelope = McpResultEnvelope::new("docs", "search", &rich_result());

    assert_eq!(
        McpResultEnvelope::from_metadata(&envelope.to_metadata()),
        Some(envelope)
    );
}

#[test]
fn an_envelope_with_another_kind_is_rejected() {
    let mut wire = McpResultEnvelope::new("docs", "search", &rich_result()).to_metadata();
    wire["kind"] = json!(MCP_CALL_RESULT_KIND);

    assert_eq!(McpResultEnvelope::from_metadata(&wire), None);
    assert_eq!(McpResultEnvelope::from_metadata(&json!("text")), None);
}

#[test]
fn an_outcome_is_read_from_an_envelope() {
    let failed = McpCallOutcome::failed("docs", "search", McpCallError::new("code"));
    let envelope =
        McpResultEnvelope::new("docs", "search", &rich_result()).with_outcome(failed.clone());

    assert_eq!(
        McpCallOutcome::from_metadata(&envelope.to_metadata()),
        Some(failed)
    );
}

#[test]
fn an_outcome_is_still_read_from_the_bare_shape() {
    let outcome = McpCallOutcome::answered("docs", "search");
    let wire = serde_json::to_value(&outcome).unwrap();

    assert_eq!(McpCallOutcome::from_metadata(&wire), Some(outcome));
}

#[test]
fn an_envelope_without_an_outcome_yields_none() {
    let envelope = McpResultEnvelope::new("docs", "search", &rich_result());

    assert_eq!(McpCallOutcome::from_metadata(&envelope.to_metadata()), None);
}

#[test]
fn an_envelope_carrying_an_invalid_outcome_yields_none() {
    let wire = json!({
        "kind": MCP_RESULT_KIND,
        "server": "docs",
        "tool": "search",
        "outcome": { "kind": MCP_CALL_RESULT_KIND, "server": "docs", "tool": "search", "ok": false },
    });

    assert_eq!(McpCallOutcome::from_metadata(&wire), None);
}
