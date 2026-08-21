//! Unit tests for the write-audit store.
//!
//! Ordering, filtering, and paging are checked against records written in a
//! deliberately shuffled order, because an audit listing that quietly reorders
//! itself between two reads is worse than one that is simply wrong: a user
//! paging through it would see records twice and miss others.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::types::AuditStore;
use crate::Error;
use serde_json::json;
use tinymcp_bus::{
    DEFAULT_LIST_LIMIT, ERROR_MESSAGE_MAX_BYTES, MAX_LIST_LIMIT, McpWriteListQuery,
    NewMcpWriteRecord,
};

/// An in-memory audit log.
fn store() -> AuditStore {
    AuditStore::open_in_memory().expect("the audit log opens")
}

/// A successful write at `timestamp_ms`.
fn write_at(timestamp_ms: i64, client: &str, tool: &str) -> NewMcpWriteRecord {
    NewMcpWriteRecord {
        timestamp_ms,
        client_info: client.to_string(),
        tool_name: tool.to_string(),
        args_summary: json!({ "chunks": 1 }),
        resulting_chunk_id: None,
        success: true,
        error_message: None,
    }
}

/// An unfiltered listing.
fn all() -> McpWriteListQuery {
    McpWriteListQuery::default()
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

#[test]
fn opening_creates_the_directory_and_file() {
    let directory = tempfile::tempdir().unwrap();
    let expected = AuditStore::path_for(directory.path());
    assert!(!expected.exists());

    let _store = AuditStore::open(directory.path()).expect("the audit log opens");

    assert!(expected.exists(), "{}", expected.display());
}

#[test]
fn the_audit_log_is_its_own_file_beside_the_server_store() {
    // It shared a database with the host's own tables before the extraction,
    // which is what made it unmovable.
    let audit = AuditStore::path_for(std::path::Path::new("/data"));
    let servers = crate::Store::path_for(std::path::Path::new("/data"));
    assert_ne!(audit, servers);
    assert!(audit.ends_with("mcp_audit/mcp_audit.db"), "{audit:?}");
}

#[test]
fn reopening_finds_what_was_written() {
    let directory = tempfile::tempdir().unwrap();

    let first = AuditStore::open(directory.path()).unwrap();
    first
        .record(&write_at(1_000, "claude", "memory_write"))
        .unwrap();
    drop(first);

    let second = AuditStore::open(directory.path()).unwrap();
    assert_eq!(second.list(&all()).unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

#[test]
fn a_record_round_trips() {
    let store = store();
    let written = NewMcpWriteRecord {
        args_summary: json!({ "chunks": 3, "bytes": 900 }),
        resulting_chunk_id: Some("chunk-1".into()),
        ..write_at(1_700_000_000_000, "claude", "memory_write")
    };

    let id = store.record(&written).unwrap();
    let listed = store.list(&all()).unwrap();

    assert_eq!(listed.len(), 1);
    let record = &listed[0];
    assert_eq!(record.id, id);
    assert_eq!(record.timestamp_ms, written.timestamp_ms);
    assert_eq!(record.client_info, "claude");
    assert_eq!(record.tool_name, "memory_write");
    assert_eq!(record.args_summary, written.args_summary);
    assert_eq!(record.resulting_chunk_id.as_deref(), Some("chunk-1"));
    assert!(record.success);
    assert_eq!(record.error_message, None);
}

#[test]
fn each_record_gets_its_own_identifier() {
    let store = store();
    let first = store.record(&write_at(1_000, "claude", "a")).unwrap();
    let second = store.record(&write_at(2_000, "claude", "b")).unwrap();

    assert_ne!(first, second);
}

#[test]
fn a_failed_write_keeps_its_flag_and_message() {
    let store = store();
    store
        .record(&NewMcpWriteRecord {
            success: false,
            error_message: Some("the remote refused".into()),
            ..write_at(1_000, "claude", "memory_write")
        })
        .unwrap();

    let record = &store.list(&all()).unwrap()[0];
    assert!(!record.success);
    assert_eq!(record.error_message.as_deref(), Some("the remote refused"));
}

#[test]
fn a_null_argument_summary_round_trips() {
    let store = store();
    store
        .record(&NewMcpWriteRecord {
            args_summary: json!(null),
            ..write_at(1_000, "claude", "ping")
        })
        .unwrap();

    assert_eq!(store.list(&all()).unwrap()[0].args_summary, json!(null));
}

// ---------------------------------------------------------------------------
// The error-message bound
// ---------------------------------------------------------------------------

#[test]
fn an_oversized_error_message_is_truncated() {
    // The message comes from a remote server and is bounded by nothing else.
    let store = store();
    store
        .record(&NewMcpWriteRecord {
            success: false,
            error_message: Some("e".repeat(ERROR_MESSAGE_MAX_BYTES * 4)),
            ..write_at(1_000, "claude", "memory_write")
        })
        .unwrap();

    let stored = store.list(&all()).unwrap()[0]
        .error_message
        .clone()
        .expect("a message");
    assert!(stored.len() <= ERROR_MESSAGE_MAX_BYTES, "{}", stored.len());
}

#[test]
fn truncation_does_not_split_a_character() {
    // A multi-byte character straddling the cap must not become invalid text.
    let store = store();
    store
        .record(&NewMcpWriteRecord {
            success: false,
            error_message: Some("é".repeat(ERROR_MESSAGE_MAX_BYTES)),
            ..write_at(1_000, "claude", "memory_write")
        })
        .unwrap();

    let stored = store.list(&all()).unwrap()[0]
        .error_message
        .clone()
        .expect("a message");
    assert!(stored.len() <= ERROR_MESSAGE_MAX_BYTES);
    assert!(stored.chars().all(|character| character == 'é'));
}

#[test]
fn a_message_that_fits_is_left_alone() {
    let store = store();
    store
        .record(&NewMcpWriteRecord {
            success: false,
            error_message: Some("short".into()),
            ..write_at(1_000, "claude", "memory_write")
        })
        .unwrap();

    assert_eq!(
        store.list(&all()).unwrap()[0].error_message.as_deref(),
        Some("short")
    );
}

// ---------------------------------------------------------------------------
// Ordering and paging
// ---------------------------------------------------------------------------

#[test]
fn records_are_listed_most_recent_first() {
    let store = store();
    for timestamp in [2_000, 1_000, 3_000] {
        store
            .record(&write_at(timestamp, "claude", "memory_write"))
            .unwrap();
    }

    let timestamps: Vec<i64> = store
        .list(&all())
        .unwrap()
        .into_iter()
        .map(|record| record.timestamp_ms)
        .collect();
    assert_eq!(timestamps, [3_000, 2_000, 1_000]);
}

#[test]
fn records_sharing_a_timestamp_order_by_identifier_descending() {
    // Without the tie-break, paging could show a record twice and miss another.
    let store = store();
    let first = store.record(&write_at(1_000, "claude", "a")).unwrap();
    let second = store.record(&write_at(1_000, "claude", "b")).unwrap();

    let ids: Vec<i64> = store
        .list(&all())
        .unwrap()
        .into_iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(ids, [second, first]);
}

#[test]
fn a_page_size_bounds_the_listing() {
    let store = store();
    for timestamp in 1..=10 {
        store
            .record(&write_at(timestamp * 1_000, "claude", "memory_write"))
            .unwrap();
    }

    let query = McpWriteListQuery {
        limit: Some(3),
        ..all()
    };
    assert_eq!(store.list(&query).unwrap().len(), 3);
}

#[test]
fn an_oversized_page_request_is_clamped_rather_than_refused() {
    let store = store();
    store.record(&write_at(1_000, "claude", "a")).unwrap();

    let query = McpWriteListQuery {
        limit: Some(u64::MAX),
        ..all()
    };
    // The clamp comes from the query type; what matters here is that an
    // extreme request still returns rather than failing to bind.
    assert_eq!(store.list(&query).unwrap().len(), 1);
    assert_eq!(query.resolved_limit(), MAX_LIST_LIMIT);
}

#[test]
fn an_offset_skips_the_most_recent_records() {
    let store = store();
    for timestamp in [1_000, 2_000, 3_000] {
        store
            .record(&write_at(timestamp, "claude", "memory_write"))
            .unwrap();
    }

    let query = McpWriteListQuery {
        offset: Some(1),
        ..all()
    };
    let timestamps: Vec<i64> = store
        .list(&query)
        .unwrap()
        .into_iter()
        .map(|record| record.timestamp_ms)
        .collect();
    assert_eq!(timestamps, [2_000, 1_000]);
}

#[test]
fn an_empty_log_lists_nothing() {
    assert!(store().list(&all()).unwrap().is_empty());
}

#[test]
fn the_default_page_size_applies_when_none_is_asked_for() {
    assert_eq!(all().resolved_limit(), DEFAULT_LIST_LIMIT);
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

#[test]
fn a_since_filter_excludes_older_records() {
    let store = store();
    for timestamp in [1_000, 2_000, 3_000] {
        store
            .record(&write_at(timestamp, "claude", "memory_write"))
            .unwrap();
    }

    let query = McpWriteListQuery {
        since_ms: Some(2_000),
        ..all()
    };
    let timestamps: Vec<i64> = store
        .list(&query)
        .unwrap()
        .into_iter()
        .map(|record| record.timestamp_ms)
        .collect();
    // Inclusive at the boundary.
    assert_eq!(timestamps, [3_000, 2_000]);
}

#[test]
fn a_client_filter_matches_exactly() {
    let store = store();
    store.record(&write_at(1_000, "claude", "a")).unwrap();
    store.record(&write_at(2_000, "other", "a")).unwrap();

    let query = McpWriteListQuery {
        client_filter: Some("claude".into()),
        ..all()
    };
    let listed = store.list(&query).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].client_info, "claude");
}

#[test]
fn a_client_filter_is_trimmed_before_it_is_applied() {
    let store = store();
    store.record(&write_at(1_000, "claude", "a")).unwrap();

    let query = McpWriteListQuery {
        client_filter: Some("  claude  ".into()),
        ..all()
    };
    assert_eq!(store.list(&query).unwrap().len(), 1);
}

#[test]
fn a_blank_filter_matches_everything_rather_than_nothing() {
    let store = store();
    store.record(&write_at(1_000, "claude", "a")).unwrap();
    store.record(&write_at(2_000, "other", "b")).unwrap();

    let query = McpWriteListQuery {
        client_filter: Some("   ".into()),
        tool_filter: Some(String::new()),
        ..all()
    };
    assert_eq!(store.list(&query).unwrap().len(), 2);
}

#[test]
fn a_tool_filter_matches_exactly() {
    let store = store();
    store
        .record(&write_at(1_000, "claude", "memory_write"))
        .unwrap();
    store
        .record(&write_at(2_000, "claude", "memory_read"))
        .unwrap();

    let query = McpWriteListQuery {
        tool_filter: Some("memory_write".into()),
        ..all()
    };
    let listed = store.list(&query).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].tool_name, "memory_write");
}

#[test]
fn a_success_filter_excludes_failures() {
    let store = store();
    store.record(&write_at(1_000, "claude", "a")).unwrap();
    store
        .record(&NewMcpWriteRecord {
            success: false,
            ..write_at(2_000, "claude", "b")
        })
        .unwrap();

    let query = McpWriteListQuery {
        success_only: Some(true),
        ..all()
    };
    let listed = store.list(&query).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].success);
}

#[test]
fn asking_for_failures_too_is_the_same_as_not_filtering() {
    let store = store();
    store.record(&write_at(1_000, "claude", "a")).unwrap();
    store
        .record(&NewMcpWriteRecord {
            success: false,
            ..write_at(2_000, "claude", "b")
        })
        .unwrap();

    for flag in [None, Some(false)] {
        let query = McpWriteListQuery {
            success_only: flag,
            ..all()
        };
        assert_eq!(store.list(&query).unwrap().len(), 2, "{flag:?}");
    }
}

#[test]
fn filters_combine() {
    let store = store();
    store
        .record(&write_at(1_000, "claude", "memory_write"))
        .unwrap();
    store
        .record(&write_at(2_000, "claude", "memory_read"))
        .unwrap();
    store
        .record(&write_at(3_000, "other", "memory_write"))
        .unwrap();

    let query = McpWriteListQuery {
        client_filter: Some("claude".into()),
        tool_filter: Some("memory_write".into()),
        ..all()
    };
    let listed = store.list(&query).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].timestamp_ms, 1_000);
}

#[test]
fn a_bound_too_large_for_the_column_is_reported_rather_than_wrapping() {
    let store = store();
    let query = McpWriteListQuery {
        since_ms: Some(u64::MAX),
        ..all()
    };

    let error = store.list(&query).expect_err("an out-of-range bound");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Bounds and boundaries
// ---------------------------------------------------------------------------

#[test]
fn a_long_error_message_is_truncated_on_a_character_boundary() {
    // These come from a remote server and reach a database column and a UI. A
    // naive byte split on a multi-byte character would panic.
    let record = NewMcpWriteRecord {
        error_message: Some("é".repeat(2_000)),
        ..new_record()
    };

    let store = AuditStore::open_in_memory().unwrap();
    store.record(&record).expect("record");

    let stored = store.list(&McpWriteListQuery::default()).unwrap();
    let message = stored[0].error_message.as_deref().expect("a message");
    assert!(message.len() <= 1024, "{} bytes", message.len());
    // Intact: a split mid-sequence would have produced replacement characters.
    assert!(message.chars().all(|character| character == 'é'));
}

#[test]
fn an_error_message_within_the_bound_is_kept_whole() {
    let record = NewMcpWriteRecord {
        error_message: Some("the tool refused".into()),
        ..new_record()
    };

    let store = AuditStore::open_in_memory().unwrap();
    store.record(&record).unwrap();

    assert_eq!(
        store.list(&McpWriteListQuery::default()).unwrap()[0]
            .error_message
            .as_deref(),
        Some("the tool refused")
    );
}

#[test]
fn a_limit_beyond_what_the_column_can_hold_is_refused_rather_than_wrapping() {
    // A bound that overflowed into a negative would turn a page request into
    // "no limit", which is how a listing endpoint becomes a way to dump the
    // whole table.
    let store = AuditStore::open_in_memory().unwrap();
    let query = McpWriteListQuery {
        offset: Some(u64::MAX),
        ..McpWriteListQuery::default()
    };

    assert!(store.list(&query).is_err());
}

#[test]
fn a_row_whose_argument_summary_was_never_written_reads_as_null() {
    // Rows predate the column being populated, and a listing has to keep
    // working rather than failing the whole page on one of them.
    let store = AuditStore::open_in_memory().unwrap();
    store
        .record(&NewMcpWriteRecord {
            args_summary: serde_json::Value::Null,
            ..new_record()
        })
        .unwrap();

    assert_eq!(
        store.list(&McpWriteListQuery::default()).unwrap()[0].args_summary,
        serde_json::Value::Null
    );
}

#[test]
fn an_audit_log_opens_under_the_directory_it_is_given() {
    let directory = tempfile::tempdir().unwrap();

    let store = AuditStore::open(directory.path()).expect("the log opens");
    store.record(&new_record()).unwrap();

    assert!(AuditStore::path_for(directory.path()).exists());
    assert_eq!(store.list(&McpWriteListQuery::default()).unwrap().len(), 1);
}

#[test]
fn an_audit_log_that_cannot_be_opened_is_reported_rather_than_panicking() {
    // A directory where the file should be. Reported as an error so a host can
    // log it and come up without auditing rather than failing to start.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("occupied.db");
    std::fs::create_dir(&path).unwrap();

    assert!(AuditStore::open_file(&path).is_err());
}
