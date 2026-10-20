//! Tests for the v1 to v2 steps, run through the registry over v1-shaped fixtures.

use super::*;

use serde_json::{Value, json};
use tinystoragedrivers_core::schema::{MigrateOptions, MigrationReport, Registry};
use tinystoragedrivers_core::{
    CollectionSpec, DocumentStoreExt as _, Filter, MemoryStorage, Precondition, Query, Scope,
    ScopedStorage, StorageBackend, Versioned,
};

use crate::storage::schema::{SCHEMA_VERSION, storage_schema};

/// A memory backend holding v1 data, and the registry that migrates it.
struct Fixture {
    blocking: tokio::runtime::Runtime,
    storage: ScopedStorage,
    registry: Registry,
}

impl Fixture {
    fn new() -> Self {
        let blocking = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let backend = MemoryStorage::new();
        let registry = Registry::compose([storage_schema()]).unwrap();
        blocking.block_on(registry.prepare(&backend)).unwrap();
        let scope = Scope::local().module("tinymcp").unwrap();
        let storage = backend.for_scope(&scope).unwrap();
        Self {
            blocking,
            storage,
            registry,
        }
    }

    /// Writes one v1 document the way the previous release did.
    fn legacy(&self, collection: &str, id: &str, body: Value) {
        let documents = self.storage.documents().clone();
        let collection = collection.to_string();
        let id = id.to_string();
        self.blocking
            .block_on(async move {
                documents
                    .ensure_collection(&CollectionSpec::new(&collection))
                    .await?;
                documents
                    .put(&collection, &id, body, Precondition::None)
                    .await
            })
            .unwrap();
    }

    fn legacy_stream(&self, name: &str, entries: &[Value]) {
        let streams = self.storage.streams().clone();
        let name = name.to_string();
        let entries = entries.to_vec();
        self.blocking
            .block_on(async move {
                for entry in entries {
                    streams.append(&name, entry).await?;
                }
                Ok::<_, tinystoragedrivers_core::StorageError>(())
            })
            .unwrap();
    }

    fn migrate(&self) -> MigrationReport {
        self.blocking
            .block_on(self.registry.migrate(&self.storage, &MigrateOptions::new()))
            .unwrap()
    }

    fn get(&self, collection: &str, id: &str) -> Option<Versioned<Value>> {
        let documents = self.storage.documents().clone();
        let collection = collection.to_string();
        let id = id.to_string();
        self.blocking
            .block_on(async move { documents.get(&collection, &id).await })
            .unwrap()
    }

    fn all(&self, collection: &str) -> Vec<Versioned<Value>> {
        let documents = self.storage.documents().clone();
        let collection = collection.to_string();
        self.blocking
            .block_on(async move { documents.query_all(&collection, &Query::all()).await })
            .unwrap()
    }

    fn count_where(&self, collection: &str, filter: Filter) -> u64 {
        let documents = self.storage.documents().clone();
        let collection = collection.to_string();
        self.blocking
            .block_on(async move { documents.count(&collection, &filter).await })
            .unwrap()
    }

    fn stream(&self, name: &str) -> Vec<Value> {
        let streams = self.storage.streams().clone();
        let name = name.to_string();
        self.blocking
            .block_on(async move {
                let len = streams.len(&name).await?;
                let window = streams.read_window(&name, 0, 1024).await?;
                assert_eq!(window.len() as u64, len);
                Ok::<Vec<Value>, tinystoragedrivers_core::StorageError>(
                    window.into_iter().map(|entry| entry.value).collect(),
                )
            })
            .unwrap()
    }
}

fn v1_server() -> Value {
    json!({
        "server_id": "srv-1",
        "qualified_name": "@acme/files",
        "display_name": "Files",
        "command_kind": "node",
        "command": "npx",
        "args": ["-y", "files"],
        "env_keys": ["TOKEN"],
        "installed_at": 1_700_000_000_000_i64,
        "last_connected_at": 1_700_000_100_000_i64,
        "transport": "stdio",
        "enabled": true,
        "catalog_managed": true,
    })
}

#[test]
fn a_v1_server_moves_to_the_v2_collection_with_ms_time_fields() {
    let fixture = Fixture::new();
    fixture.legacy("mcp_servers", "srv-1", v1_server());

    fixture.migrate();

    let moved = fixture.get(SERVERS, "srv-1").expect("the server moved");
    assert_eq!(moved.doc["installed_ms"], 1_700_000_000_000_i64);
    assert_eq!(moved.doc["last_connected_ms"], 1_700_000_100_000_i64);
    assert!(moved.doc.get("installed_at").is_none());
    assert!(moved.doc.get("last_connected_at").is_none());
    assert_eq!(moved.doc["qualified_name"], "@acme/files");
    assert_eq!(moved.doc["catalog_managed"], true);
    assert_eq!(moved.doc["args"], json!(["-y", "files"]));
    assert_eq!(moved.meta.schema, SCHEMA_VERSION);
    assert_eq!(fixture.all("mcp_servers").len(), 0);
}

#[test]
fn a_server_never_connected_keeps_no_last_connected_field() {
    let fixture = Fixture::new();
    let mut server = v1_server();
    server.as_object_mut().unwrap().remove("last_connected_at");
    fixture.legacy("mcp_servers", "srv-1", server);

    fixture.migrate();

    let moved = fixture.get(SERVERS, "srv-1").unwrap();
    assert!(moved.doc.get("last_connected_ms").is_none());
    assert_eq!(moved.doc["installed_ms"], 1_700_000_000_000_i64);
}

#[test]
fn a_server_id_with_a_slash_is_escaped_into_its_v2_key() {
    let fixture = Fixture::new();
    fixture.legacy("mcp_servers", "a/b", v1_server());

    fixture.migrate();

    assert!(fixture.get(SERVERS, "a%2Fb").is_some());
}

#[test]
fn a_name_claim_is_keyed_by_the_qualified_name_and_keeps_its_time() {
    let fixture = Fixture::new();
    fixture.legacy(
        "mcp_server_names",
        "legacy-claim-files",
        json!({"qualified_name": "@acme/files", "server_id": "srv-1", "claimed_at": 5_000}),
    );
    fixture.legacy(
        "mcp_server_names",
        "legacy-claim-old",
        json!({"qualified_name": "@acme/old", "server_id": "srv-0"}),
    );

    fixture.migrate();

    let claim = fixture.get(SERVER_NAMES, "@acme%2Ffiles").unwrap();
    assert_eq!(
        claim.doc,
        json!({"qualified_name": "@acme/files", "server_id": "srv-1", "claimed_ms": 5_000})
    );
    // A claim that predates the time reads as long expired.
    let old = fixture.get(SERVER_NAMES, "@acme%2Fold").unwrap();
    assert_eq!(old.doc["claimed_ms"], 0);
    assert_eq!(fixture.all("mcp_server_names").len(), 0);
}

#[test]
fn a_cached_browse_response_becomes_an_object_body_with_a_cached_time() {
    let fixture = Fixture::new();
    fixture.legacy(
        "mcp_registry_cache",
        "digest-1",
        json!({"cache_key": "official:search:slack", "body_json": "{\"servers\":[1,2]}", "cached_at": 9_000}),
    );

    fixture.migrate();

    let moved = fixture
        .get(REGISTRY_CACHE, "official:search:slack")
        .unwrap();
    assert_eq!(
        moved.doc,
        json!({"cache_key": "official:search:slack", "body": {"servers": [1, 2]}, "cached_ms": 9_000})
    );
}

#[test]
fn a_cached_response_that_is_not_json_is_dropped_rather_than_blocking_the_step() {
    let fixture = Fixture::new();
    fixture.legacy(
        "mcp_registry_cache",
        "digest-1",
        json!({"cache_key": "k", "body_json": "not json", "cached_at": 1}),
    );
    fixture.legacy(
        "mcp_registry_cache",
        "digest-2",
        json!({"cache_key": "ok", "body_json": "[]", "cached_at": 2}),
    );

    fixture.migrate();

    assert!(fixture.get(REGISTRY_CACHE, "k").is_none());
    assert!(fixture.get(REGISTRY_CACHE, "ok").is_some());
    assert_eq!(fixture.all("mcp_registry_cache").len(), 0);
}

#[test]
fn cached_tools_are_keyed_by_the_server_key() {
    let fixture = Fixture::new();
    fixture.legacy(
        "mcp_tool_cache",
        "digest-1",
        json!({"server_key": "static:docs", "fingerprint": "abc", "tools": [{"name": "t"}], "cached_at": 7_000}),
    );

    fixture.migrate();

    let moved = fixture.get(TOOL_CACHE, "static:docs").unwrap();
    assert_eq!(
        moved.doc,
        json!({"server_key": "static:docs", "fingerprint": "abc", "tools": [{"name": "t"}], "cached_ms": 7_000})
    );
}

#[test]
fn an_index_row_gets_an_object_record_a_synced_time_and_a_composite_key() {
    let fixture = Fixture::new();
    fixture.legacy(
        "mcp_registry_index",
        "digest-1",
        json!({
            "source": "official",
            "qualified_name": "@acme/files",
            "label": "files",
            "description": "read files",
            "record_json": "{\"name\":\"files\"}",
            "synced_at": 4_000,
            "search": "fi fil file files",
        }),
    );

    fixture.migrate();

    let moved = fixture
        .get(REGISTRY_INDEX, "official/@acme%2Ffiles")
        .unwrap();
    assert_eq!(moved.doc["record"], json!({"name": "files"}));
    assert_eq!(moved.doc["synced_ms"], 4_000);
    assert!(moved.doc.get("record_json").is_none());
    assert!(moved.doc.get("synced_at").is_none());
    assert_eq!(moved.doc["search"], "fi fil file files");
    assert_eq!(
        fixture.count_where(REGISTRY_INDEX, Filter::eq("source", "official")),
        1
    );
}

#[test]
fn an_index_state_moves_its_times_to_ms_fields() {
    let fixture = Fixture::new();
    fixture.legacy(
        "mcp_registry_index_state",
        "digest-1",
        json!({"source": "official", "base_url": "https://r", "cursor": "c", "pages": 3, "started_at": 100, "synced_at": 50}),
    );
    fixture.legacy(
        "mcp_registry_index_state",
        "digest-2",
        json!({"source": "other", "base_url": "https://o", "pages": 0}),
    );

    fixture.migrate();

    let state = fixture.get(REGISTRY_INDEX_STATE, "official").unwrap();
    assert_eq!(
        state.doc,
        json!({"source": "official", "base_url": "https://r", "cursor": "c", "pages": 3, "started_ms": 100, "synced_ms": 50})
    );
    let idle = fixture.get(REGISTRY_INDEX_STATE, "other").unwrap();
    assert!(idle.doc.get("started_ms").is_none());
    assert!(idle.doc.get("synced_ms").is_none());
}

#[test]
fn the_audit_stream_moves_in_order_under_its_v2_name() {
    let fixture = Fixture::new();
    let entries = [
        json!({"timestamp_ms": 1, "client_info": "c", "tool_name": "a", "args_summary": {}, "success": true}),
        json!({"timestamp_ms": 2, "client_info": "c", "tool_name": "b", "args_summary": {}, "success": false, "error_message": "boom"}),
    ];
    fixture.legacy_stream("mcp_audit", &entries);

    fixture.migrate();

    assert_eq!(fixture.stream(AUDIT_STREAM), entries);
    assert_eq!(fixture.stream("mcp_audit").len(), 0);
}

#[test]
fn migrating_again_changes_nothing() {
    let fixture = Fixture::new();
    fixture.legacy("mcp_servers", "srv-1", v1_server());
    fixture.legacy_stream("mcp_audit", &[json!({"timestamp_ms": 1})]);

    fixture.migrate();
    let before = fixture.get(SERVERS, "srv-1").unwrap();
    fixture.migrate();

    let after = fixture.get(SERVERS, "srv-1").unwrap();
    assert_eq!(before, after);
    assert_eq!(fixture.stream(AUDIT_STREAM).len(), 1);
}

#[test]
fn the_module_can_be_used_once_every_step_has_finished() {
    let fixture = Fixture::new();
    assert!(fixture.registry.check_ready(SERVERS).is_err());

    fixture.migrate();

    fixture.registry.check_ready(SERVERS).unwrap();
    fixture.registry.check_ready(AUDIT_STREAM).unwrap();
}
