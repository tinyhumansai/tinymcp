//! Unit tests for the installed-server store.
//!
//! Two things here are load-bearing beyond ordinary persistence.
//!
//! **The records.** A store reopened over the same ports must find what an
//! earlier one wrote, and a record written before a field existed must load
//! with that field's old meaning. Those tests seed raw documents deliberately,
//! because that is the only way to prove an upgrade works rather than a fresh
//! install.
//!
//! **The credential boundary.** Values live in the secret store and must not
//! reach a record that gets serialized, or a document at all. That is asserted
//! directly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use serde_json::json;
use tinystoragedrivers_core::Filter;

use super::types::Store;
use crate::Error;
use crate::storage::{
    CACHE_KEY_SECRET, ENV_SECRET_PREFIX, REGISTRY_CACHE, SCHEMA_VERSION, SERVER_NAMES, SERVERS,
    StoragePorts, TOOL_CACHE, doc_id,
};
use tinymcp_bus::{CommandKind, InstalledServer, McpTool, Transport};

/// A store over fresh in-memory ports.
fn store() -> Store {
    Store::open_in_memory().expect("the store opens")
}

/// Ports plus a store over them, for tests that reopen or inspect.
fn shared() -> (StoragePorts, Store) {
    let ports = StoragePorts::memory().unwrap();
    let store = Store::open(ports.clone()).expect("the store opens");
    (ports, store)
}

/// A subprocess install.
fn stdio_server(id: &str) -> InstalledServer {
    InstalledServer {
        server_id: id.to_string(),
        qualified_name: "@test/server".to_string(),
        display_name: "Test Server".to_string(),
        description: Some("A test server".to_string()),
        icon_url: None,
        command_kind: CommandKind::Node,
        command: "npx".to_string(),
        args: vec!["-y".to_string(), "@test/server".to_string()],
        env_keys: vec!["API_KEY".to_string()],
        config: None,
        installed_at: 1_700_000_000_000,
        last_connected_at: None,
        transport: Transport::Stdio,
        enabled: true,
    }
}

/// An HTTP-remote install.
fn http_server(id: &str, url: &str) -> InstalledServer {
    InstalledServer {
        qualified_name: "@test/http-server".to_string(),
        display_name: "Test HTTP Server".to_string(),
        description: None,
        command: String::new(),
        args: Vec::new(),
        env_keys: Vec::new(),
        transport: Transport::HttpRemote {
            url: url.to_string(),
        },
        ..stdio_server(id)
    }
}

#[test]
fn tool_cache_round_trips_and_requires_the_same_fingerprint() {
    let store = store();
    let tools = [McpTool::new("search")];
    store
        .put_cached_tools("srv-1", "definition-a", &tools)
        .unwrap();
    let cached = store
        .cached_tools("srv-1", "definition-a")
        .unwrap()
        .unwrap();
    assert_eq!(cached.tools, tools);
    assert!(
        store
            .cached_tools("srv-1", "definition-b")
            .unwrap()
            .is_none()
    );
    assert!(store.forget_cached_tools("srv-1").unwrap());
    assert!(!store.forget_cached_tools("srv-1").unwrap());
}

#[test]
fn persisted_tool_cache_fingerprints_are_keyed_and_stable_after_reopen() {
    let (ports, store) = shared();
    let fingerprint = crate::registry::store::fingerprint(&["basic", "user", "password123"]);
    let tools = [McpTool::new("search")];
    store
        .put_cached_tools("static:weather", &fingerprint, &tools)
        .unwrap();

    let id = doc_id(&["static:weather"]).unwrap();
    let persisted = store.get_raw(TOOL_CACHE, &id).unwrap();
    let persisted = persisted["fingerprint"].as_str().unwrap().to_string();
    assert_ne!(persisted, fingerprint);
    assert_ne!(
        persisted,
        crate::registry::store::fingerprint(&[&fingerprint]),
        "the persisted value must not be an unkeyed digest of guessable input"
    );
    drop(store);

    let reopened = Store::open(ports).unwrap();
    assert_eq!(format!("{reopened:?}"), "Store { .. }");
    let cached = reopened
        .cached_tools("static:weather", &fingerprint)
        .unwrap()
        .unwrap();
    assert_eq!(cached.tools, tools);
    let rotated = crate::registry::store::fingerprint(&["basic", "user", "password456"]);
    assert!(
        reopened
            .cached_tools("static:weather", &rotated)
            .unwrap()
            .is_none(),
        "credential changes must still invalidate the persisted catalog"
    );
}

#[test]
fn the_cache_key_is_kept_as_a_secret_and_never_in_a_document() {
    let (ports, store) = shared();
    let key = ports
        .run("reading the key", |ports| async move {
            ports.secrets.get(CACHE_KEY_SECRET).await
        })
        .unwrap()
        .expect("the key is stored as a secret");
    assert_eq!(key.len(), 64, "32 bytes of hex");
    let fingerprint = store.keyed_cache_fingerprint("definition");
    assert!(!fingerprint.contains(std::str::from_utf8(&key).unwrap()));
}

#[test]
fn concurrent_store_opens_share_one_cache_fingerprint_key() {
    let ports = StoragePorts::memory().unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let ports = ports.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                Store::open(ports).unwrap().keyed_cache_fingerprint("same")
            })
        })
        .collect();
    let keyed: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    // Every opener reads back whatever key won, so a store opened after the
    // race agrees with the stored one.
    let settled = Store::open(ports).unwrap().keyed_cache_fingerprint("same");
    assert!(keyed.iter().any(|value| value == &settled));
}

#[test]
fn keyed_cache_fingerprint_matches_the_hmac_sha256_vector() {
    assert_eq!(
        super::tool_cache::keyed_fingerprint(&[11; 32], "Hi There"),
        "198a607eb44bfbc69903a0f1cf2bbdc5ba0aa3f3d9ae3c1c7a3b1696a0b68cf7"
    );
}

#[test]
fn a_malformed_stored_cache_key_is_reported_when_opening() {
    let ports = StoragePorts::memory().unwrap();
    ports
        .run("seeding the key", |ports| async move {
            ports.secrets.set(CACHE_KEY_SECRET, b"too short").await
        })
        .unwrap();
    assert!(matches!(
        Store::open(ports),
        Err(crate::Error::Store { .. })
    ));
}

#[test]
fn tool_cache_rejects_corrupt_json() {
    let store = store();
    let fingerprint = store.keyed_cache_fingerprint("f");
    store.put_raw(
        TOOL_CACHE,
        &doc_id(&["srv-1"]).unwrap(),
        json!({"server_key": "srv-1", "fingerprint": fingerprint, "tools": "bad", "cached_ms": 1}),
    );
    assert!(store.cached_tools("srv-1", "f").is_err());
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

#[test]
fn opening_declares_every_collection_the_module_uses() {
    let (ports, _store) = shared();
    for spec in tinymcp_bus::storage_needs().needs.collections {
        let collection = spec.name.clone();
        ports
            .run("counting", move |ports| async move {
                ports.documents.count(&collection, &Filter::default()).await
            })
            .unwrap();
    }
}

#[test]
fn opening_the_same_ports_twice_finds_the_same_data() {
    let (ports, first) = shared();
    first.insert_server(&stdio_server("srv-1")).unwrap();
    drop(first);

    let second = Store::open(ports).unwrap();
    assert_eq!(second.list_servers().unwrap().len(), 1);
}

#[test]
fn an_in_memory_store_works_without_a_filesystem() {
    let store = Store::open_in_memory().expect("an in-memory store");
    store.insert_server(&stdio_server("srv-1")).unwrap();
    assert_eq!(store.list_servers().unwrap().len(), 1);
}

#[test]
fn the_store_exposes_the_ports_it_writes_through() {
    let (ports, store) = shared();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    assert!(ports.has_state().unwrap());
    assert!(store.ports().has_state().unwrap());
}

// ---------------------------------------------------------------------------
// Installed servers
// ---------------------------------------------------------------------------

#[test]
fn a_server_round_trips_through_the_store() {
    let store = store();
    let server = stdio_server("srv-1");

    store.insert_server(&server).unwrap();

    assert_eq!(store.get_server("srv-1").unwrap(), server);
}

#[test]
fn servers_are_listed_oldest_install_first() {
    let store = store();
    store
        .insert_server(&InstalledServer {
            installed_at: 3_000,
            qualified_name: "@test/c".into(),
            ..stdio_server("srv-c")
        })
        .unwrap();
    store
        .insert_server(&InstalledServer {
            installed_at: 1_000,
            qualified_name: "@test/a".into(),
            ..stdio_server("srv-a")
        })
        .unwrap();

    let ids: Vec<String> = store
        .list_servers()
        .unwrap()
        .into_iter()
        .map(|server| server.server_id)
        .collect();
    assert_eq!(ids, ["srv-a", "srv-c"]);
}

#[test]
fn arguments_and_environment_names_survive_the_round_trip() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    let loaded = store.get_server("srv-1").unwrap();
    assert_eq!(loaded.args, ["-y", "@test/server"]);
    assert_eq!(loaded.env_keys, ["API_KEY"]);
}

#[test]
fn an_http_remote_install_keeps_its_endpoint() {
    let store = store();
    store
        .insert_server(&http_server("srv-http", "https://x.test/mcp"))
        .unwrap();

    assert_eq!(
        store.get_server("srv-http").unwrap().transport,
        Transport::HttpRemote {
            url: "https://x.test/mcp".into()
        }
    );
}

#[test]
fn each_row_keeps_its_own_transport() {
    let store = store();
    store.insert_server(&stdio_server("srv-stdio")).unwrap();
    store
        .insert_server(&http_server("srv-http", "https://x.test/mcp"))
        .unwrap();

    let listed = store.list_servers().unwrap();
    let stdio = listed.iter().find(|s| s.server_id == "srv-stdio").unwrap();
    let http = listed.iter().find(|s| s.server_id == "srv-http").unwrap();

    assert_eq!(stdio.transport, Transport::Stdio);
    assert!(matches!(http.transport, Transport::HttpRemote { .. }));
}

#[test]
fn an_absent_server_is_reported_by_name() {
    let store = store();

    let error = store.get_server("nope").expect_err("no such install");

    assert!(
        matches!(error, Error::UnknownServer { ref server } if server == "nope"),
        "{error:?}"
    );
    assert!(store.find_server("nope").unwrap().is_none());
}

#[test]
fn deleting_reports_whether_a_row_went() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    assert!(store.delete_server("srv-1").unwrap());
    assert!(!store.delete_server("srv-1").unwrap());
}

#[test]
fn deleting_a_server_takes_its_credentials_with_it() {
    // The cascade is what stops a credential outliving the install it belongs
    // to and being handed to whatever reuses the identifier.
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([("API_KEY".to_string(), "secret".to_string())]),
        )
        .unwrap();

    store.delete_server("srv-1").unwrap();

    assert!(store.load_env_values("srv-1").unwrap().is_empty());
}

#[test]
fn installing_if_absent_deduplicates_on_the_qualified_name() {
    let store = store();

    assert!(
        store
            .insert_server_if_absent(&stdio_server("srv-1"))
            .unwrap()
    );
    // Same qualified name, different identifier: the second must not land.
    assert!(
        !store
            .insert_server_if_absent(&stdio_server("srv-2"))
            .unwrap()
    );

    assert_eq!(store.list_servers().unwrap().len(), 1);
}

#[test]
fn catalog_provenance_is_saved_only_for_catalog_installs() {
    let store = store();
    store.insert_server(&stdio_server("user-1")).unwrap();
    assert!(!store.is_catalog_server("user-1").unwrap());
    assert!(
        store
            .insert_catalog_server_if_absent(&InstalledServer {
                qualified_name: "@catalog/server".into(),
                ..stdio_server("catalog-1")
            })
            .unwrap()
    );
    assert!(store.is_catalog_server("catalog-1").unwrap());
}

#[test]
fn a_lookup_by_qualified_name_returns_the_earliest_install() {
    let store = store();
    store
        .insert_server(&InstalledServer {
            installed_at: 2_000,
            ..stdio_server("srv-later")
        })
        .unwrap();
    store
        .insert_server(&InstalledServer {
            installed_at: 1_000,
            ..stdio_server("srv-earlier")
        })
        .unwrap();

    let found = store
        .find_server_by_qualified_name("@test/server")
        .unwrap()
        .expect("an install");
    assert_eq!(found.server_id, "srv-earlier");
}

#[test]
fn a_lookup_for_an_uninstalled_service_finds_nothing() {
    let store = store();
    assert!(
        store
            .find_server_by_qualified_name("@nobody/nothing")
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Updates
// ---------------------------------------------------------------------------

#[test]
fn a_configuration_blob_round_trips_and_clears() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    assert_eq!(store.get_server("srv-1").unwrap().config, None);

    store
        .update_config("srv-1", Some(&json!({ "region": "eu" })))
        .unwrap();
    assert_eq!(
        store.get_server("srv-1").unwrap().config,
        Some(json!({ "region": "eu" }))
    );

    store.update_config("srv-1", None).unwrap();
    assert_eq!(store.get_server("srv-1").unwrap().config, None);
}

#[test]
fn environment_names_can_be_replaced_without_touching_the_rest_of_the_row() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    store
        .update_env_keys("srv-1", &["API_KEY".into(), "REGION".into()])
        .unwrap();

    let loaded = store.get_server("srv-1").unwrap();
    assert_eq!(loaded.env_keys, ["API_KEY", "REGION"]);
    assert_eq!(loaded.display_name, "Test Server");
}

#[test]
fn the_enabled_flag_defaults_true_and_can_be_flipped() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    assert!(store.get_server("srv-1").unwrap().enabled);

    store.update_enabled("srv-1", false).unwrap();
    assert!(!store.get_server("srv-1").unwrap().enabled);

    store.update_enabled("srv-1", true).unwrap();
    assert!(store.get_server("srv-1").unwrap().enabled);
}

#[test]
fn a_disabled_server_is_stored_as_disabled() {
    let store = store();
    store
        .insert_server(&InstalledServer {
            enabled: false,
            ..stdio_server("srv-1")
        })
        .unwrap();

    assert!(!store.get_server("srv-1").unwrap().enabled);
}

#[test]
fn recording_a_connection_sets_a_timestamp_where_there_was_none() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    assert_eq!(store.get_server("srv-1").unwrap().last_connected_at, None);

    store.touch_last_connected("srv-1").unwrap();

    assert!(
        store
            .get_server("srv-1")
            .unwrap()
            .last_connected_at
            .is_some()
    );
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[test]
fn credentials_round_trip() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    let env = BTreeMap::from([
        ("API_KEY".to_string(), "secret".to_string()),
        ("REGION".to_string(), "eu".to_string()),
    ]);
    store.set_env_values("srv-1", &env).unwrap();

    assert_eq!(store.load_env_values("srv-1").unwrap(), env);
}

#[test]
fn replacing_credentials_removes_the_ones_that_are_gone() {
    // A name dropped from the new set must stop being handed to the server,
    // not linger from the previous write.
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([
                ("OLD".to_string(), "1".to_string()),
                ("KEPT".to_string(), "1".to_string()),
            ]),
        )
        .unwrap();
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([("KEPT".to_string(), "2".to_string())]),
        )
        .unwrap();

    let loaded = store.load_env_values("srv-1").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded.get("KEPT").map(String::as_str), Some("2"));
}

#[test]
fn a_server_with_no_credentials_loads_an_empty_set() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    assert!(store.load_env_values("srv-1").unwrap().is_empty());
}

#[test]
fn a_credential_value_never_reaches_the_install_record() {
    // The record is listed over the bus, rendered in interfaces, and logged.
    // The values must not be reachable from it at all.
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([("API_KEY".to_string(), "super-secret".to_string())]),
        )
        .unwrap();

    let record = store.get_server("srv-1").unwrap();
    let serialized = serde_json::to_string(&record).unwrap();

    assert!(serialized.contains("API_KEY"), "the name should be there");
    assert!(
        !serialized.contains("super-secret"),
        "a credential value reached the install record: {serialized}"
    );
}

// ---------------------------------------------------------------------------
// Browse cache
// ---------------------------------------------------------------------------

#[test]
fn an_empty_cache_misses() {
    let store = store();
    assert_eq!(store.cached("any-key").unwrap(), None);
}

#[test]
fn a_freshly_written_entry_hits() {
    let store = store();
    store
        .cache("servers?q=weather", r#"{"servers":[]}"#)
        .unwrap();

    assert_eq!(
        store.cached("servers?q=weather").unwrap().as_deref(),
        Some(r#"{"servers":[]}"#)
    );
}

#[test]
fn writing_the_same_key_replaces_the_previous_body() {
    let store = store();
    store.cache("key", r#""first""#).unwrap();
    store.cache("key", r#""second""#).unwrap();

    assert_eq!(store.cached("key").unwrap().as_deref(), Some(r#""second""#));
}

#[test]
fn an_entry_older_than_the_lifetime_misses() {
    // Backdated directly, so the test does not have to wait ten minutes.
    let store = store();
    store.cache("key", r#""stale""#).unwrap();

    age_cache(&store, 11);

    assert_eq!(store.cached("key").unwrap(), None);
}

/// Moves every cached entry `minutes` into the past.
fn age_cache(store: &Store, minutes: i64) {
    store.age_collection(REGISTRY_CACHE, &["cached_ms"], minutes * 60 * 1_000);
}

#[test]
fn a_stale_read_returns_an_entry_past_its_lifetime() {
    let store = store();
    store.cache("key", r#""old""#).unwrap();
    age_cache(&store, 60 * 24);

    assert_eq!(store.cached("key").unwrap(), None);
    assert_eq!(
        store.cached_stale("key").unwrap().as_deref(),
        Some(r#""old""#)
    );
    assert_eq!(store.cached_stale("other").unwrap(), None);
}

#[test]
fn a_prefix_read_returns_matching_entries_newest_first() {
    let store = store();
    store.cache("browse:1", r#""older""#).unwrap();
    age_cache(&store, 30);
    store.cache("browse:2", r#""newer""#).unwrap();
    store.cache("search:x:1", r#""unrelated""#).unwrap();

    assert_eq!(
        store.cached_with_prefix("browse:").unwrap(),
        vec![r#""newer""#.to_string(), r#""older""#.to_string()]
    );
}

#[test]
fn a_prefix_read_treats_like_wildcards_literally() {
    let store = store();
    store.cache("a_b:1", r#""literal""#).unwrap();
    store.cache("axb:1", r#""wildcard match""#).unwrap();

    assert_eq!(
        store.cached_with_prefix("a_b:").unwrap(),
        vec![r#""literal""#.to_string()]
    );
    assert_eq!(
        store.cached_with_prefix("nothing:").unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn cache_keys_are_independent() {
    let store = store();
    store.cache("a", r#""first""#).unwrap();

    assert_eq!(store.cached("a").unwrap().as_deref(), Some(r#""first""#));
    assert_eq!(store.cached("b").unwrap(), None);
}

// ---------------------------------------------------------------------------
// Records that predate a field, and records that are damaged
// ---------------------------------------------------------------------------

/// A server document as the first release wrote it: no transport, enabled or
/// provenance fields.
fn seed_original_record(store: &Store) {
    store.put_raw(
        SERVERS,
        "legacy-1",
        json!({
            "server_id": "legacy-1",
            "qualified_name": "@old/server",
            "display_name": "Old Server",
            "command_kind": "node",
            "command": "npx",
            "installed_ms": 1000,
        }),
    );
}

#[test]
fn legacy_installs_keep_unknown_catalog_provenance() {
    let store = store();
    seed_original_record(&store);
    assert!(!store.is_catalog_server("legacy-1").unwrap());
}

#[test]
fn a_record_from_before_the_transport_field_loads_as_a_subprocess_install() {
    // Every install predating that field was a subprocess install. Reading it
    // as anything else would misroute it on the next connect.
    let store = store();
    seed_original_record(&store);

    let loaded = store.get_server("legacy-1").unwrap();

    assert_eq!(loaded.transport, Transport::Stdio);
    assert_eq!(loaded.args, Vec::<String>::new());
    assert_eq!(loaded.env_keys, Vec::<String>::new());
}

#[test]
fn a_record_from_before_the_enabled_field_loads_as_enabled() {
    // Those installs were auto-connecting. Loading them as disabled would
    // silently stop a user's servers on upgrade.
    let store = store();
    seed_original_record(&store);

    assert!(store.get_server("legacy-1").unwrap().enabled);
}

#[test]
fn a_record_whose_fields_are_unreadable_fails_the_read_rather_than_being_guessed_at() {
    // `args` and `env_keys` are lists. A value that does not decode is
    // corruption, and inventing an empty list would silently drop a server's
    // arguments or hide which credentials it holds.
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    let mut doc = store.get_raw(SERVERS, "srv-1").unwrap();
    doc["args"] = json!("not a list");
    store.put_raw(SERVERS, "srv-1", doc);

    assert!(store.list_servers().is_err());
    assert!(store.get_server("srv-1").is_err());
}

#[test]
fn a_record_whose_configuration_is_unreadable_fails_the_read() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    let mut doc = store.get_raw(SERVERS, "srv-1").unwrap();
    doc["config"] = json!(7);
    store.put_raw(SERVERS, "srv-1", doc);
    // A configuration is any JSON value, so this one reads; a record that is
    // not an object at all does not.
    assert!(store.get_server("srv-1").is_ok());
    store.put_raw(SERVERS, "srv-1", json!({"server_id": "srv-1"}));
    assert!(store.get_server("srv-1").is_err());
}

// ---------------------------------------------------------------------------
// Credentials and claims
// ---------------------------------------------------------------------------

#[test]
fn credential_values_live_in_one_secret_and_never_in_a_document() {
    let (ports, store) = shared();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([("API_KEY".to_string(), "super-secret".to_string())]),
        )
        .unwrap();

    let secret = ports
        .run("reading the secret", |ports| async move {
            ports
                .secrets
                .get(&format!("{ENV_SECRET_PREFIX}srv-1"))
                .await
        })
        .unwrap()
        .expect("the values are a secret");
    assert!(String::from_utf8_lossy(&secret).contains("super-secret"));

    let server = store.get_raw(SERVERS, "srv-1").unwrap().to_string();
    assert!(!server.contains("super-secret"), "{server}");
}

#[test]
fn clearing_every_credential_removes_the_secret() {
    let (ports, store) = shared();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([("API_KEY".to_string(), "v".to_string())]),
        )
        .unwrap();
    store.set_env_values("srv-1", &BTreeMap::new()).unwrap();

    let secret = ports
        .run("reading the secret", |ports| async move {
            ports
                .secrets
                .get(&format!("{ENV_SECRET_PREFIX}srv-1"))
                .await
        })
        .unwrap();
    assert!(secret.is_none());
}

#[test]
fn a_secret_that_is_not_a_credential_map_fails_the_load() {
    let (ports, store) = shared();
    ports
        .run("seeding", |ports| async move {
            ports
                .secrets
                .set(&format!("{ENV_SECRET_PREFIX}srv-1"), b"[1, 2]")
                .await
        })
        .unwrap();
    assert!(store.load_env_values("srv-1").is_err());
}

#[test]
fn deleting_a_server_releases_its_qualified_name() {
    let store = store();
    assert!(
        store
            .insert_server_if_absent(&stdio_server("srv-1"))
            .unwrap()
    );
    assert!(
        store
            .get_raw(SERVER_NAMES, &doc_id(&["@test/server"]).unwrap())
            .is_some()
    );

    store.delete_server("srv-1").unwrap();

    assert!(
        store
            .get_raw(SERVER_NAMES, &doc_id(&["@test/server"]).unwrap())
            .is_none()
    );
    assert!(
        store
            .insert_server_if_absent(&stdio_server("srv-2"))
            .unwrap()
    );
}

#[test]
fn a_claim_whose_server_is_gone_is_taken_over() {
    // A crash between the claim and the server write leaves a claim with no
    // install behind it; it must not block that service forever.
    let store = store();
    store.put_raw(
        SERVER_NAMES,
        &doc_id(&["@test/server"]).unwrap(),
        json!({"qualified_name": "@test/server", "server_id": "ghost", "claimed_at": 1}),
    );
    assert!(
        store
            .insert_server_if_absent(&stdio_server("srv-1"))
            .unwrap()
    );
    assert_eq!(store.list_servers().unwrap().len(), 1);
}

#[test]
fn a_fresh_claim_without_its_server_is_in_flight_not_stale() {
    // Another installer may be between its claim and its server write.
    let store = store();
    store.put_raw(
        SERVER_NAMES,
        &doc_id(&["@test/server"]).unwrap(),
        json!({
            "qualified_name": "@test/server",
            "server_id": "in-flight",
            "claimed_ms": super::now_ms(),
        }),
    );
    assert!(
        !store
            .insert_server_if_absent(&stdio_server("srv-1"))
            .unwrap()
    );
    assert_eq!(store.list_servers().unwrap().len(), 0);
}

#[test]
fn concurrent_installs_of_one_service_make_exactly_one_install() {
    let store = std::sync::Arc::new(store());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    let threads: Vec<_> = (0..6)
        .map(|index| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .insert_server_if_absent(&stdio_server(&format!("srv-{index}")))
                    .unwrap()
            })
        })
        .collect();
    let inserted = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .filter(|inserted| *inserted)
        .count();
    assert_eq!(inserted, 1);
    assert_eq!(store.list_servers().unwrap().len(), 1);
}

#[test]
fn a_plain_insert_of_a_taken_identifier_is_refused() {
    let store = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    assert!(matches!(
        store.insert_server(&stdio_server("srv-1")),
        Err(crate::Error::Store { .. })
    ));
}

#[test]
fn updating_a_server_that_is_not_installed_changes_nothing() {
    let store = store();
    store.update_enabled("nobody", false).unwrap();
    store.touch_last_connected("nobody").unwrap();
    store.update_config("nobody", Some(&json!({}))).unwrap();
    store.update_env_keys("nobody", &["K".to_string()]).unwrap();
    assert_eq!(store.list_servers().unwrap().len(), 0);
}

#[test]
fn concurrent_updates_to_one_server_all_land() {
    let store = std::sync::Arc::new(store());
    store.insert_server(&stdio_server("srv-1")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let threads: Vec<_> = (0..3)
        .map(|index| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                match index {
                    0 => store.update_enabled("srv-1", false).unwrap(),
                    1 => store.touch_last_connected("srv-1").unwrap(),
                    _ => store
                        .update_config("srv-1", Some(&json!({"k": 1})))
                        .unwrap(),
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let server = store.get_server("srv-1").unwrap();
    assert!(!server.enabled);
    assert!(server.last_connected_at.is_some());
    assert_eq!(server.config, Some(json!({"k": 1})));
}

// ---------------------------------------------------------------------------
// The v2 stored form
// ---------------------------------------------------------------------------

/// The metadata the driver recorded for a document.
fn meta_of(
    ports: &StoragePorts,
    collection: &'static str,
    id: &str,
) -> tinystoragedrivers_core::Meta {
    let id = id.to_string();
    ports
        .run("reading metadata", move |ports| async move {
            ports.documents.get(collection, &id).await
        })
        .unwrap()
        .expect("the document exists")
        .meta
}

#[test]
fn an_installed_server_is_stored_with_ms_times_and_the_declared_schema() {
    let (ports, store) = shared();
    let mut server = stdio_server("srv-1");
    server.last_connected_at = Some(1_700_000_100_000);
    store.insert_server(&server).unwrap();

    let doc = store.get_raw(SERVERS, "srv-1").unwrap();
    assert_eq!(doc["installed_ms"], 1_700_000_000_000_i64);
    assert_eq!(doc["last_connected_ms"], 1_700_000_100_000_i64);
    assert!(doc.get("installed_at").is_none());
    assert!(doc.get("last_connected_at").is_none());
    assert_eq!(meta_of(&ports, SERVERS, "srv-1").schema, SCHEMA_VERSION);
    assert_eq!(store.get_server("srv-1").unwrap(), server);
}

#[test]
fn touching_the_last_connection_keeps_the_declared_schema() {
    let (ports, store) = shared();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    store.touch_last_connected("srv-1").unwrap();

    assert!(
        store
            .get_server("srv-1")
            .unwrap()
            .last_connected_at
            .is_some()
    );
    assert_eq!(meta_of(&ports, SERVERS, "srv-1").schema, SCHEMA_VERSION);
}

#[test]
fn a_name_claim_is_keyed_by_the_escaped_qualified_name() {
    let store = store();
    assert!(
        store
            .insert_server_if_absent(&stdio_server("srv-1"))
            .unwrap()
    );

    let claim = store.get_raw(SERVER_NAMES, "@test%2Fserver").unwrap();
    assert_eq!(claim["server_id"], "srv-1");
    assert!(claim["claimed_ms"].as_i64().unwrap() > 0);
    assert!(claim.get("claimed_at").is_none());
}

#[test]
fn a_cached_response_is_stored_as_a_json_body_not_a_string() {
    let store = store();
    store.cache("browse:1", r#"{"servers":[1]}"#).unwrap();

    let doc = store.get_raw(REGISTRY_CACHE, "browse:1").unwrap();
    assert_eq!(doc["body"], json!({"servers": [1]}));
    assert!(doc["cached_ms"].as_i64().unwrap() > 0);
    assert!(doc.get("body_json").is_none());
}

#[test]
fn a_response_that_is_not_json_is_refused() {
    let store = store();
    let error = store.cache("browse:1", "not json").unwrap_err();
    assert!(matches!(error, Error::Serialization { .. }), "{error:?}");
    assert_eq!(store.cached("browse:1").unwrap(), None);
}

#[test]
fn cached_tools_are_stored_with_a_cached_time_under_the_escaped_server_key() {
    let store = store();
    store
        .put_cached_tools("static:weather", "definition", &[McpTool::new("search")])
        .unwrap();

    let doc = store.get_raw(TOOL_CACHE, "static:weather").unwrap();
    assert!(doc["cached_ms"].as_i64().unwrap() > 0);
    assert!(doc.get("cached_at").is_none());
}
