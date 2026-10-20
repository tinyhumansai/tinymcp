//! Unit tests for the catalog index tables.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const SOURCE: &str = "mcp_official";
const BASE: &str = "https://registry.test";

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn row(name: &str, label: &str, description: &str) -> IndexRow {
    IndexRow {
        qualified_name: name.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        record_json: format!("{{\"name\":\"{name}\"}}"),
    }
}

fn terms(words: &[&str]) -> Vec<String> {
    words.iter().map(ToString::to_string).collect()
}

fn names(hits: &[IndexHit]) -> Vec<&str> {
    let mut names: Vec<&str> = hits.iter().map(|hit| hit.qualified_name.as_str()).collect();
    names.sort_unstable();
    names
}

/// Moves every timestamp of the index back by a day.
fn age(store: &Store) {
    store.age_collection(crate::storage::REGISTRY_INDEX, &["synced_ms"], 86_400_000);
    store.age_collection(
        crate::storage::REGISTRY_INDEX_STATE,
        &["synced_ms"],
        86_400_000,
    );
}

#[test]
fn a_store_with_no_sync_has_no_index_state() {
    assert_eq!(store().index_state(SOURCE).unwrap(), None);
}

#[test]
fn a_sync_records_each_page_and_its_cursor() {
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    store
        .store_index_page(SOURCE, &[row("a/one", "a/one one", "first")], Some("2"))
        .unwrap();

    let state = store.index_state(SOURCE).unwrap().unwrap();
    assert_eq!(state.base_url, BASE);
    assert_eq!(state.cursor.as_deref(), Some("2"));
    assert_eq!(state.pages, 1);
    assert!(state.in_progress());
    assert_eq!(state.synced_ms, None);
}

#[test]
fn finishing_a_sync_records_when_and_clears_the_resume_point() {
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    store
        .store_index_page(SOURCE, &[row("a/one", "a/one", "")], None)
        .unwrap();
    store.finish_index_sync(SOURCE).unwrap();

    let state = store.index_state(SOURCE).unwrap().unwrap();
    assert!(!state.in_progress());
    assert!(state.synced_ms.is_some());
    assert_eq!(state.cursor, None);
    assert_eq!(state.pages, 0);
}

#[test]
fn finishing_a_sync_drops_servers_it_did_not_see_again() {
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    store
        .store_index_page(
            SOURCE,
            &[row("a/kept", "kept", ""), row("a/gone", "gone", "")],
            None,
        )
        .unwrap();
    store.finish_index_sync(SOURCE).unwrap();
    age(&store);

    store.begin_index_sync(SOURCE, BASE).unwrap();
    store
        .store_index_page(SOURCE, &[row("a/kept", "kept", "")], None)
        .unwrap();
    assert_eq!(
        names(&store.search_index(SOURCE, &[]).unwrap()),
        ["a/gone", "a/kept"],
        "an unfinished sync keeps the earlier rows searchable"
    );

    store.finish_index_sync(SOURCE).unwrap();
    assert_eq!(names(&store.search_index(SOURCE, &[]).unwrap()), ["a/kept"]);
}

#[test]
fn restarting_on_the_same_catalog_keeps_the_last_finished_sync() {
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    store.finish_index_sync(SOURCE).unwrap();

    store.begin_index_sync(SOURCE, BASE).unwrap();

    assert!(
        store
            .index_state(SOURCE)
            .unwrap()
            .unwrap()
            .synced_ms
            .is_some()
    );
}

#[test]
fn a_sync_of_a_different_catalog_drops_the_old_rows() {
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    store
        .store_index_page(SOURCE, &[row("a/one", "one", "")], None)
        .unwrap();
    store.finish_index_sync(SOURCE).unwrap();

    store
        .begin_index_sync(SOURCE, "https://elsewhere.test")
        .unwrap();

    let state = store.index_state(SOURCE).unwrap().unwrap();
    assert_eq!(state.base_url, "https://elsewhere.test");
    assert_eq!(state.synced_ms, None);
    assert_eq!(
        names(&store.search_index(SOURCE, &[]).unwrap()),
        Vec::<&str>::new()
    );
}

#[test]
fn a_search_needs_every_term_in_the_label_or_the_description() {
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    store
        .store_index_page(
            SOURCE,
            &[
                row("a/slack", "a/slack slack", "post messages"),
                row("a/chat", "a/chat chat", "a slack bridge"),
                row("a/mail", "a/mail mail", "send email"),
            ],
            None,
        )
        .unwrap();

    let hits = store.search_index(SOURCE, &terms(&["slack"])).unwrap();
    assert_eq!(names(&hits), ["a/chat", "a/slack"]);

    let in_label: Vec<bool> = {
        let mut hits = hits;
        hits.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
        hits.iter().map(|hit| hit.in_label).collect()
    };
    assert_eq!(in_label, [false, true]);

    let both = store
        .search_index(SOURCE, &terms(&["slack", "messages"]))
        .unwrap();
    assert_eq!(names(&both), ["a/slack"]);
}

#[test]
fn a_search_is_scoped_to_its_source() {
    let store = store();
    store.begin_index_sync("other", BASE).unwrap();
    store
        .store_index_page("other", &[row("a/one", "one", "")], None)
        .unwrap();

    assert_eq!(
        names(&store.search_index(SOURCE, &terms(&["one"])).unwrap()),
        Vec::<&str>::new()
    );
}

#[test]
fn a_page_or_finish_without_a_begun_sync_changes_nothing() {
    let store = store();
    store
        .store_index_page(SOURCE, &[row("a/one", "a/one one", "first")], None)
        .unwrap();
    store.finish_index_sync(SOURCE).unwrap();
    assert_eq!(store.index_state(SOURCE).unwrap(), None);
}

#[test]
fn a_word_longer_than_the_prefix_limit_is_found_by_its_full_text() {
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    store
        .store_index_page(
            SOURCE,
            &[row(
                "a/long",
                "a/long extraordinarilylongidentifier",
                "desc",
            )],
            None,
        )
        .unwrap();
    store.finish_index_sync(SOURCE).unwrap();
    let hits = store
        .search_index(SOURCE, &terms(&["extraordinarilylongidentifier"]))
        .unwrap();
    assert_eq!(names(&hits), vec!["a/long"]);
}

#[test]
fn a_backend_without_transactions_still_writes_every_document_of_a_chunk() {
    let documents = crate::storage::StoragePorts::memory().unwrap().documents;
    let ops = vec![
        WriteOp::Put {
            collection: "c".to_string(),
            id: "1".to_string(),
            doc: serde_json::json!({"n": 1}),
            precondition: Precondition::None,
        },
        WriteOp::Put {
            collection: "c".to_string(),
            id: "2".to_string(),
            doc: serde_json::json!({"n": 2}),
            precondition: Precondition::None,
        },
    ];
    futures_executor_block(write_chunk(documents.as_ref(), ops, false)).unwrap();
    for id in ["1", "2"] {
        assert!(
            futures_executor_block(documents.get("c", id))
                .unwrap()
                .is_some()
        );
    }
}

fn futures_executor_block<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn a_sync_state_that_cannot_be_read_fails_the_page_and_the_finish() {
    let store = store();
    store.put_raw(
        crate::storage::REGISTRY_INDEX_STATE,
        &crate::storage::doc_id(&[SOURCE]).unwrap(),
        serde_json::json!({"unexpected": true}),
    );
    assert!(
        store
            .store_index_page(SOURCE, &[row("a/one", "a/one one", "first")], None)
            .is_err()
    );
    assert!(store.finish_index_sync(SOURCE).is_err());
}

#[test]
fn a_term_inside_a_word_still_finds_its_row() {
    // The text index carries word prefixes only; `ack` is not a prefix of
    // `slack`, yet the documented contract is substring matching.
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    store
        .store_index_page(SOURCE, &[row("a/slack", "a/slack slack", "")], None)
        .unwrap();

    let hits = store.search_index(SOURCE, &terms(&["ack"])).unwrap();
    assert_eq!(names(&hits), ["a/slack"]);
}

#[test]
fn an_index_row_is_stored_with_an_object_record_and_a_synced_time() {
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    store
        .store_index_page(SOURCE, &[row("a/one", "a/one one", "first")], None)
        .unwrap();

    let doc = store
        .get_raw(crate::storage::REGISTRY_INDEX, "mcp_official/a%2Fone")
        .unwrap();
    assert_eq!(doc["record"], serde_json::json!({"name": "a/one"}));
    assert!(doc["synced_ms"].as_i64().unwrap() > 0);
    assert!(doc.get("record_json").is_none());
    let state = store
        .get_raw(crate::storage::REGISTRY_INDEX_STATE, SOURCE)
        .unwrap();
    assert!(state["started_ms"].as_i64().unwrap() > 0);
}

#[test]
fn an_index_row_whose_record_is_not_json_is_refused() {
    let store = store();
    store.begin_index_sync(SOURCE, BASE).unwrap();
    let mut bad = row("a/one", "a/one one", "first");
    bad.record_json = "not json".to_string();
    assert!(store.store_index_page(SOURCE, &[bad], None).is_err());
}
