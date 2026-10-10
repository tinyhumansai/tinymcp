//! Unit tests for the operation reply types.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::{RegistryFreshness, RegistrySearchPage, SearchCuration, ServerDetail, ToolCallOutcome};
use crate::{McpServerToolResult, McpToolResult};

#[test]
fn a_tool_outcome_carries_the_raw_reply_and_its_rendering() {
    let raw = json!({ "content": [{ "type": "text", "text": "sunny" }] });
    let outcome = ToolCallOutcome::from(McpServerToolResult::new(
        raw.clone(),
        McpToolResult::success("sunny"),
    ));

    assert_eq!(outcome.result, raw);
    assert!(!outcome.is_error);
    assert_eq!(
        outcome.rendered.content,
        vec![crate::McpToolContent::Text {
            text: "sunny".into()
        }]
    );
}

#[test]
fn a_failing_tool_outcome_takes_its_error_flag_from_the_rendering() {
    let outcome = ToolCallOutcome::from(McpServerToolResult::new(
        json!({ "isError": true }),
        McpToolResult::error("bad argument"),
    ));

    assert!(outcome.is_error);
    assert!(outcome.rendered.is_error);
}

#[test]
fn a_tool_outcome_from_an_older_module_decodes_with_an_empty_rendering() {
    // Contract 1.0 sent `result` and `is_error` only. `rendered` is additive,
    // so those frames must keep decoding.
    let outcome: ToolCallOutcome =
        serde_json::from_value(json!({ "result": { "ok": true }, "is_error": false })).unwrap();

    assert_eq!(outcome.rendered, McpToolResult::default());
}

#[test]
fn a_tool_outcome_serializes_its_rendering_under_rendered() {
    let outcome = ToolCallOutcome::from(McpServerToolResult::new(
        json!({}),
        McpToolResult::success("done"),
    ));
    let wire = serde_json::to_value(&outcome).unwrap();

    assert_eq!(wire["is_error"], json!(false));
    assert_eq!(wire["result"], json!({}));
    assert_eq!(
        wire["rendered"]["content"],
        json!([{ "type": "text", "text": "done" }]),
    );
}

#[test]
fn a_server_detail_serializes_as_the_detail_and_its_env_keys() {
    let detail: ServerDetail = serde_json::from_value(json!({
        "server": { "qualified_name": "com.notion/mcp", "display_name": "Notion" },
        "required_env_keys": ["NOTION_TOKEN"],
    }))
    .unwrap();

    assert_eq!(detail.server.qualified_name, "com.notion/mcp");
    assert_eq!(detail.required_env_keys, ["NOTION_TOKEN"]);

    let wire = serde_json::to_value(&detail).unwrap();
    assert_eq!(wire["required_env_keys"], json!(["NOTION_TOKEN"]));
    assert_eq!(wire["server"]["qualified_name"], json!("com.notion/mcp"));
}

#[test]
fn a_server_detail_requires_its_env_keys() {
    // Unchanged from when the type lived in the module crate: no default.
    let decoded = serde_json::from_value::<ServerDetail>(json!({
        "server": { "qualified_name": "a", "display_name": "A" },
    }));

    assert!(decoded.is_err());
}

#[test]
fn an_empty_curation_request_asks_for_no_curation() {
    let curation: SearchCuration = serde_json::from_value(json!({})).unwrap();

    assert_eq!(curation, SearchCuration::default());
    assert!(!curation.tag_official);
    assert!(!curation.official_first);
}

#[test]
fn a_curation_request_serializes_both_switches() {
    let curation = SearchCuration {
        tag_official: true,
        official_first: false,
    };

    assert_eq!(
        serde_json::to_value(curation).unwrap(),
        json!({ "tag_official": true, "official_first": false }),
    );
}

#[test]
fn freshness_travels_in_snake_case() {
    for (freshness, wire) in [
        (RegistryFreshness::Live, "live"),
        (RegistryFreshness::Indexed, "indexed"),
        (RegistryFreshness::Cached, "cached"),
        (RegistryFreshness::LocalFallback, "local_fallback"),
    ] {
        assert_eq!(serde_json::to_value(freshness).unwrap(), json!(wire));
        assert_eq!(
            serde_json::from_value::<RegistryFreshness>(json!(wire)).unwrap(),
            freshness
        );
    }
}

#[test]
fn freshness_orders_from_freshest_to_least_fresh() {
    assert!(RegistryFreshness::Live < RegistryFreshness::Indexed);
    assert!(RegistryFreshness::Indexed < RegistryFreshness::Cached);
    assert!(RegistryFreshness::Cached < RegistryFreshness::LocalFallback);
    assert_eq!(
        RegistryFreshness::Live.max(RegistryFreshness::Indexed),
        RegistryFreshness::Indexed
    );
    assert_eq!(
        RegistryFreshness::Live.max(RegistryFreshness::LocalFallback),
        RegistryFreshness::LocalFallback
    );
}

#[test]
fn a_search_page_from_an_older_module_reads_as_live() {
    let page: RegistrySearchPage =
        serde_json::from_value(json!({ "servers": [], "page": 1, "total_pages": 1 })).unwrap();

    assert_eq!(page.freshness, RegistryFreshness::Live);
}

#[test]
fn a_search_page_serializes_its_freshness() {
    let page = RegistrySearchPage {
        freshness: RegistryFreshness::Cached,
        ..RegistrySearchPage::default()
    };

    assert_eq!(
        serde_json::to_value(&page).unwrap()["freshness"],
        json!("cached")
    );
}
