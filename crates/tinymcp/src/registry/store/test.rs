//! Unit tests for the installed-server store.
//!
//! Two things here are load-bearing beyond ordinary persistence.
//!
//! **The migrations.** A user upgrading across a release opens a file written
//! by an older build. Those tests create the *old* schema deliberately and then
//! open a store over it, because that is the only way to prove a real upgrade
//! works rather than proving a fresh install does.
//!
//! **The credential boundary.** Values live in their own table and must not
//! reach a record that gets serialized. That is asserted directly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde_json::json;

use super::schema;
use super::types::Store;
use crate::Error;
use tinymcp_bus::{CommandKind, InstalledServer, Transport};

/// A store over a fresh temporary file.
///
/// A file rather than memory, so the migration tests can reopen the same
/// database and the rest exercise the path a real host takes.
fn store() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).expect("the store opens");
    (directory, store)
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

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

#[test]
fn opening_creates_the_directory_and_file() {
    let directory = tempfile::tempdir().unwrap();
    let expected = Store::path_for(directory.path());
    assert!(!expected.exists());

    let _store = Store::open(directory.path()).expect("the store opens");

    assert!(expected.exists(), "{}", expected.display());
}

#[test]
fn the_filename_is_unchanged_from_before_the_extraction() {
    // A user upgrading must find their existing installs, not an empty store
    // beside an orphaned file.
    let path = Store::path_for(std::path::Path::new("/data"));
    assert!(path.ends_with("mcp_clients/mcp_clients.db"), "{path:?}");
}

#[test]
fn opening_the_same_directory_twice_finds_the_same_data() {
    let directory = tempfile::tempdir().unwrap();

    let first = Store::open(directory.path()).unwrap();
    first.insert_server(&stdio_server("srv-1")).unwrap();
    drop(first);

    let second = Store::open(directory.path()).unwrap();
    assert_eq!(second.list_servers().unwrap().len(), 1);
}

#[test]
fn an_in_memory_store_works_without_a_filesystem() {
    let store = Store::open_in_memory().expect("an in-memory store");
    store.insert_server(&stdio_server("srv-1")).unwrap();
    assert_eq!(store.list_servers().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Installed servers
// ---------------------------------------------------------------------------

#[test]
fn a_server_round_trips_through_the_store() {
    let (_directory, store) = store();
    let server = stdio_server("srv-1");

    store.insert_server(&server).unwrap();

    assert_eq!(store.get_server("srv-1").unwrap(), server);
}

#[test]
fn servers_are_listed_oldest_install_first() {
    let (_directory, store) = store();
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
    let (_directory, store) = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    let loaded = store.get_server("srv-1").unwrap();
    assert_eq!(loaded.args, ["-y", "@test/server"]);
    assert_eq!(loaded.env_keys, ["API_KEY"]);
}

#[test]
fn an_http_remote_install_keeps_its_endpoint() {
    let (_directory, store) = store();
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
    let (_directory, store) = store();
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
    let (_directory, store) = store();

    let error = store.get_server("nope").expect_err("no such install");

    assert!(
        matches!(error, Error::UnknownServer { ref server } if server == "nope"),
        "{error:?}"
    );
    assert!(store.find_server("nope").unwrap().is_none());
}

#[test]
fn deleting_reports_whether_a_row_went() {
    let (_directory, store) = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    assert!(store.delete_server("srv-1").unwrap());
    assert!(!store.delete_server("srv-1").unwrap());
}

#[test]
fn deleting_a_server_takes_its_credentials_with_it() {
    // The cascade is what stops a credential outliving the install it belongs
    // to and being handed to whatever reuses the identifier.
    let (_directory, store) = store();
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
    let (_directory, store) = store();

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
fn a_lookup_by_qualified_name_returns_the_earliest_install() {
    let (_directory, store) = store();
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
    let (_directory, store) = store();
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
    let (_directory, store) = store();
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
    let (_directory, store) = store();
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
    let (_directory, store) = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    assert!(store.get_server("srv-1").unwrap().enabled);

    store.update_enabled("srv-1", false).unwrap();
    assert!(!store.get_server("srv-1").unwrap().enabled);

    store.update_enabled("srv-1", true).unwrap();
    assert!(store.get_server("srv-1").unwrap().enabled);
}

#[test]
fn a_disabled_server_is_stored_as_disabled() {
    let (_directory, store) = store();
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
    let (_directory, store) = store();
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
    let (_directory, store) = store();
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
    let (_directory, store) = store();
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
    let (_directory, store) = store();
    store.insert_server(&stdio_server("srv-1")).unwrap();

    assert!(store.load_env_values("srv-1").unwrap().is_empty());
}

#[test]
fn a_credential_value_never_reaches_the_install_record() {
    // The record is listed over the bus, rendered in interfaces, and logged.
    // The values must not be reachable from it at all.
    let (_directory, store) = store();
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
    let (_directory, store) = store();
    assert_eq!(store.cached("any-key").unwrap(), None);
}

#[test]
fn a_freshly_written_entry_hits() {
    let (_directory, store) = store();
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
    let (_directory, store) = store();
    store.cache("key", "first").unwrap();
    store.cache("key", "second").unwrap();

    assert_eq!(store.cached("key").unwrap().as_deref(), Some("second"));
}

#[test]
fn an_entry_older_than_the_lifetime_misses() {
    // Backdated directly, so the test does not have to wait ten minutes.
    let (_directory, store) = store();
    store.cache("key", "stale").unwrap();

    store.with_connection(|connection| {
        connection
            .execute(
                "UPDATE mcp_registry_cache SET cached_at = cached_at - ?1",
                rusqlite::params![11 * 60 * 1_000i64],
            )
            .unwrap();
    });

    assert_eq!(store.cached("key").unwrap(), None);
}

#[test]
fn cache_keys_are_independent() {
    let (_directory, store) = store();
    store.cache("a", "first").unwrap();

    assert_eq!(store.cached("a").unwrap().as_deref(), Some("first"));
    assert_eq!(store.cached("b").unwrap(), None);
}

// ---------------------------------------------------------------------------
// Migrations
// ---------------------------------------------------------------------------

/// Writes the original schema — before the three additive columns — with one
/// row in it, exactly as an older build would have left the file.
fn write_pre_migration_database(path: &std::path::Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE mcp_servers (
                 server_id           TEXT PRIMARY KEY,
                 qualified_name      TEXT NOT NULL,
                 display_name        TEXT NOT NULL,
                 description         TEXT,
                 icon_url            TEXT,
                 command_kind        TEXT NOT NULL DEFAULT 'node',
                 command             TEXT NOT NULL,
                 args_json           TEXT NOT NULL DEFAULT '[]',
                 env_keys_json       TEXT NOT NULL DEFAULT '[]',
                 config_json         TEXT,
                 installed_at        INTEGER NOT NULL,
                 last_connected_at   INTEGER
             );
             INSERT INTO mcp_servers
                 (server_id, qualified_name, display_name, command, installed_at)
             VALUES ('legacy-1', '@old/server', 'Old Server', 'npx', 1000);",
        )
        .unwrap();
}

#[test]
fn an_older_database_gains_the_columns_it_is_missing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp_clients.db");
    write_pre_migration_database(&path);

    let store = Store::open_file(&path).expect("the older file opens");

    let columns =
        store.with_connection(|connection| schema::columns_of(connection, "mcp_servers").unwrap());
    for column in ["transport", "deployment_url", "enabled"] {
        assert!(
            columns.iter().any(|name| name == column),
            "missing {column}"
        );
    }
}

#[test]
fn a_row_from_before_the_transport_column_loads_as_a_subprocess_install() {
    // Every install predating that column was a subprocess install. Reading it
    // as anything else would misroute it on the next connect.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp_clients.db");
    write_pre_migration_database(&path);

    let store = Store::open_file(&path).unwrap();
    let loaded = store.get_server("legacy-1").unwrap();

    assert_eq!(loaded.transport, Transport::Stdio);
}

#[test]
fn a_row_from_before_the_enabled_column_loads_as_enabled() {
    // Those installs were auto-connecting. Loading them as disabled would
    // silently stop a user's servers on upgrade.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp_clients.db");
    write_pre_migration_database(&path);

    let store = Store::open_file(&path).unwrap();

    assert!(store.get_server("legacy-1").unwrap().enabled);
}

#[test]
fn migrating_is_idempotent_across_reopens() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp_clients.db");
    write_pre_migration_database(&path);

    for _ in 0..3 {
        let store = Store::open_file(&path).expect("reopening a migrated file");
        assert_eq!(store.list_servers().unwrap().len(), 1);
    }
}

#[test]
fn a_duplicate_column_failure_is_treated_as_success() {
    // Two processes holding the same file can each see a column as missing and
    // both try to add it. The loser must not report an error: the column
    // existing is the outcome wanted.
    let store = Store::open_in_memory().unwrap();

    store.with_connection(|connection| {
        schema::add_column(
            connection,
            "ALTER TABLE mcp_servers ADD COLUMN transport TEXT NOT NULL DEFAULT 'stdio'",
            "transport",
        )
        .expect("a duplicate column is not an error");
    });
}

#[test]
fn any_other_migration_failure_still_propagates() {
    // Swallowing the duplicate-column case must not turn into swallowing
    // everything.
    let store = Store::open_in_memory().unwrap();

    store.with_connection(|connection| {
        let error = schema::add_column(
            connection,
            "ALTER TABLE table_that_does_not_exist ADD COLUMN x TEXT",
            "x",
        )
        .expect_err("a missing table is a real failure");
        assert!(matches!(error, Error::Store { .. }), "{error:?}");
    });
}

// ---------------------------------------------------------------------------
// Opening, and rows that predate a column
// ---------------------------------------------------------------------------

#[test]
fn a_store_that_cannot_be_opened_is_reported_rather_than_panicking() {
    // A directory where the file should be. Reported so a host can log it and
    // decide, rather than failing to start on a path problem.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("occupied.db");
    std::fs::create_dir(&path).unwrap();

    assert!(Store::open_file(&path).is_err());
}

#[test]
fn a_row_whose_json_columns_are_unreadable_fails_the_read_rather_than_being_guessed_at() {
    // `args` and `env_keys` are JSON in a text column. A value that does not
    // decode is corruption, and inventing an empty list would silently drop a
    // server's arguments or hide which credentials it holds.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store.db");
    let store = Store::open_file(&path).unwrap();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    drop(store);

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE mcp_servers SET args_json = 'not json' WHERE server_id = 'srv-1'",
            [],
        )
        .unwrap();
    drop(connection);

    let store = Store::open_file(&path).unwrap();
    assert!(store.list_servers().is_err());
}

#[test]
fn a_row_whose_configuration_is_unreadable_fails_the_read() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store.db");
    let store = Store::open_file(&path).unwrap();
    store.insert_server(&stdio_server("srv-1")).unwrap();
    drop(store);

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE mcp_servers SET config_json = '{ not json' WHERE server_id = 'srv-1'",
            [],
        )
        .unwrap();
    drop(connection);

    assert!(
        Store::open_file(&path)
            .unwrap()
            .get_server("srv-1")
            .is_err()
    );
}
