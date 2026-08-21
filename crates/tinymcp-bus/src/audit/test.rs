//! Unit tests for the write-audit payload types.
//!
//! The interesting behavior here is the query's `resolved_*` accessors: they
//! are the bounds that stop an unbounded table and an unbounded remote string
//! from becoming an unbounded response, so each one is exercised at its edge
//! rather than only in the middle.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{
    DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT, McpWriteListQuery, McpWriteRecord, NewMcpWriteRecord,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[test]
fn a_new_record_round_trips() {
    let record = NewMcpWriteRecord {
        timestamp_ms: 1_700_000_000_000,
        client_info: "claude".into(),
        tool_name: "memory_write".into(),
        args_summary: json!({ "chunks": 3 }),
        resulting_chunk_id: Some("chunk-1".into()),
        success: true,
        error_message: None,
    };
    let encoded = serde_json::to_value(&record).unwrap();
    assert_eq!(
        serde_json::from_value::<NewMcpWriteRecord>(encoded).unwrap(),
        record
    );
}

#[test]
fn a_new_record_decodes_without_its_optional_fields() {
    let record: NewMcpWriteRecord = serde_json::from_value(json!({
        "timestamp_ms": 1_700_000_000_000i64,
        "client_info": "claude",
        "tool_name": "memory_write",
        "args_summary": null,
        "success": false,
    }))
    .unwrap();

    assert_eq!(record.resulting_chunk_id, None);
    assert_eq!(record.error_message, None);
    assert!(!record.success);
}

#[test]
fn a_stored_record_carries_its_row_id() {
    let record: McpWriteRecord = serde_json::from_value(json!({
        "id": 7,
        "timestamp_ms": 1_700_000_000_000i64,
        "client_info": "claude",
        "tool_name": "memory_write",
        "args_summary": { "chunks": 3 },
        "success": true,
    }))
    .unwrap();

    assert_eq!(record.id, 7);
    assert_eq!(record.args_summary, json!({ "chunks": 3 }));
}

// ---------------------------------------------------------------------------
// Query limits
// ---------------------------------------------------------------------------

#[test]
fn an_empty_query_takes_the_default_page_size() {
    assert_eq!(
        McpWriteListQuery::default().resolved_limit(),
        DEFAULT_LIST_LIMIT
    );
    assert_eq!(McpWriteListQuery::default().resolved_offset(), 0);
}

#[test]
fn a_page_size_over_the_maximum_is_clamped_to_it() {
    let mut query = McpWriteListQuery::default();
    for requested in [MAX_LIST_LIMIT + 1, 10_000, u64::MAX] {
        query.limit = Some(requested);
        assert_eq!(
            query.resolved_limit(),
            MAX_LIST_LIMIT,
            "a request for {requested} was not clamped"
        );
    }
}

#[test]
fn a_page_size_at_the_maximum_is_honored_exactly() {
    let mut query = McpWriteListQuery::default();
    query.limit = Some(MAX_LIST_LIMIT);
    assert_eq!(query.resolved_limit(), MAX_LIST_LIMIT);
}

#[test]
fn a_page_size_under_the_default_is_honored() {
    let mut query = McpWriteListQuery::default();
    query.limit = Some(1);
    assert_eq!(query.resolved_limit(), 1);
}

#[test]
fn a_zero_page_size_is_honored_rather_than_replaced_by_the_default() {
    // `unwrap_or` only fills in an absent value. An explicit zero is a caller
    // asking for a count-only query, and silently turning it into fifty rows
    // would be a surprising thing to do with an explicit request.
    let mut query = McpWriteListQuery::default();
    query.limit = Some(0);
    assert_eq!(query.resolved_limit(), 0);
}

#[test]
fn an_offset_is_passed_through_unchanged() {
    let mut query = McpWriteListQuery::default();
    query.offset = Some(1_000);
    assert_eq!(query.resolved_offset(), 1_000);
}

// ---------------------------------------------------------------------------
// Query filters
// ---------------------------------------------------------------------------

#[test]
fn a_filter_is_trimmed() {
    let mut query = McpWriteListQuery::default();
    query.client_filter = Some("  claude  ".into());
    query.tool_filter = Some("\tmemory_write\n".into());

    assert_eq!(query.resolved_client_filter(), Some("claude"));
    assert_eq!(query.resolved_tool_filter(), Some("memory_write"));
}

#[test]
fn a_blank_filter_means_no_filter_rather_than_matching_nothing() {
    // A caller sending an empty text box wants everything, not silence.
    let mut query = McpWriteListQuery::default();
    for blank in ["", "   ", "\t\n"] {
        query.client_filter = Some(blank.into());
        query.tool_filter = Some(blank.into());
        assert_eq!(query.resolved_client_filter(), None, "{blank:?}");
        assert_eq!(query.resolved_tool_filter(), None, "{blank:?}");
    }
}

#[test]
fn an_absent_filter_is_no_filter() {
    let query = McpWriteListQuery::default();
    assert_eq!(query.resolved_client_filter(), None);
    assert_eq!(query.resolved_tool_filter(), None);
}

#[test]
fn both_none_and_false_mean_do_not_filter_on_success() {
    let mut query = McpWriteListQuery::default();
    assert!(!query.resolved_success_only());

    query.success_only = Some(false);
    assert!(!query.resolved_success_only());

    query.success_only = Some(true);
    assert!(query.resolved_success_only());
}

// ---------------------------------------------------------------------------
// Query wire form
// ---------------------------------------------------------------------------

#[test]
fn a_query_decodes_from_the_empty_object() {
    let query: McpWriteListQuery = serde_json::from_value(json!({})).unwrap();
    assert_eq!(query, McpWriteListQuery::default());
}

#[test]
fn a_query_round_trips_every_field() {
    let query = McpWriteListQuery {
        limit: Some(10),
        offset: Some(20),
        since_ms: Some(1_700_000_000_000),
        client_filter: Some("claude".into()),
        tool_filter: Some("memory_write".into()),
        success_only: Some(true),
    };
    let encoded = serde_json::to_value(&query).unwrap();
    assert_eq!(
        serde_json::from_value::<McpWriteListQuery>(encoded).unwrap(),
        query
    );
}
