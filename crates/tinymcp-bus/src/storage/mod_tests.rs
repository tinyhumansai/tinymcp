//! Tests for `TinyMCP`'s canonical storage declaration.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

#[test]
fn the_contract_declares_every_collection_index_stream_and_secret_selector() {
    let declaration = storage_needs();
    let needs = declaration.needs;
    let names: Vec<_> = needs
        .collections
        .iter()
        .map(|spec| spec.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            SERVERS,
            SERVER_NAMES,
            REGISTRY_CACHE,
            TOOL_CACHE,
            REGISTRY_INDEX,
            REGISTRY_INDEX_STATE,
        ]
    );

    let indexes = |collection_name: &str| {
        needs
            .collections
            .iter()
            .find(|spec| spec.name == collection_name)
            .expect("the collection is declared")
            .indexes
            .iter()
            .map(|index| {
                (
                    index.name.as_str(),
                    index.fields.iter().map(String::as_str).collect::<Vec<_>>(),
                    index.unique,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        indexes(SERVERS),
        [
            ("qualified_name", vec!["qualified_name"], false),
            ("installed_ms", vec!["installed_ms"], false),
        ]
    );
    assert_eq!(
        indexes(REGISTRY_CACHE),
        [("cache_key", vec!["cache_key"], false)]
    );
    assert_eq!(indexes(REGISTRY_INDEX), [("source", vec!["source"], false)]);
    assert!(needs.collections.iter().all(|spec| spec.search.is_none()));
    assert_eq!(needs.blob_prefixes, &[] as &[&str]);
    assert_eq!(needs.stream_prefixes, [AUDIT_STREAM]);
    assert_eq!(needs.secret_prefixes, [ENV_SECRET_PREFIX]);
    assert_eq!(declaration.exact_secret_names, [CACHE_KEY_SECRET]);
}
