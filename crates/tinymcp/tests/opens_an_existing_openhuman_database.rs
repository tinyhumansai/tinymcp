//! Reading a database written before the extraction.
//!
//! The registry's file, `mcp_clients/mcp_clients.db`, was created by OpenHuman
//! and is not migrated on the way over: an installed server is state a user
//! set up, and losing it would mean re-authorizing every integration by hand.
//! So this crate has to open the file as it stands, including files old enough
//! to predate the `transport`, `deployment_url`, and `enabled` columns.
//!
//! The schema below is copied verbatim from OpenHuman's original
//! `mcp_clients` initialiser rather than derived from this crate's, which is
//! the point: a test built from the current schema would agree with itself no
//! matter how far the two had drifted.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use tinymcp::{CommandKind, Store, Transport};

/// The tables as OpenHuman first cut them, before any column was added.
const ORIGINAL_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS mcp_servers (
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

    CREATE TABLE IF NOT EXISTS mcp_client_env (
        server_id   TEXT NOT NULL,
        key         TEXT NOT NULL,
        value       TEXT NOT NULL,
        PRIMARY KEY (server_id, key),
        FOREIGN KEY (server_id) REFERENCES mcp_servers(server_id) ON DELETE CASCADE
    );

    CREATE TABLE IF NOT EXISTS mcp_registry_cache (
        cache_key   TEXT PRIMARY KEY,
        body_json   TEXT NOT NULL,
        cached_at   INTEGER NOT NULL
    );";

/// Writes a database in the original shape, holding one install and one stored
/// credential, at the path this crate looks for it under `data_dir`.
fn write_legacy_database(data_dir: &std::path::Path) {
    let path = Store::path_for(data_dir);
    std::fs::create_dir_all(path.parent().expect("the path has a parent")).expect("create the dir");

    let connection = rusqlite::Connection::open(&path).expect("open the legacy file");
    connection
        .execute_batch(ORIGINAL_SCHEMA)
        .expect("create the original schema");
    connection
        .execute(
            "INSERT INTO mcp_servers (
                 server_id, qualified_name, display_name, description, icon_url,
                 command_kind, command, args_json, env_keys_json, config_json,
                 installed_at, last_connected_at
             ) VALUES (
                 'srv-legacy', '@acme/weather', 'Weather', 'Forecasts', NULL,
                 'node', 'weather-mcp', '[\"--verbose\"]', '[\"API_KEY\"]', NULL,
                 1700000000, 1700000100
             )",
            [],
        )
        .expect("insert the legacy install");
    connection
        .execute(
            "INSERT INTO mcp_client_env (server_id, key, value)
             VALUES ('srv-legacy', 'API_KEY', 'sekrit')",
            [],
        )
        .expect("insert the legacy credential");
}

#[test]
fn an_install_written_before_the_extraction_is_still_listed() {
    let workspace = tempfile::tempdir().expect("tempdir");
    write_legacy_database(workspace.path());

    let store = Store::open(workspace.path()).expect("the legacy file opens");
    let servers = store.list_servers().expect("list");

    assert_eq!(servers.len(), 1, "{servers:?}");
    let server = &servers[0];
    assert_eq!(server.server_id, "srv-legacy");
    assert_eq!(server.qualified_name, "@acme/weather");
    assert_eq!(server.display_name, "Weather");
    assert_eq!(server.description.as_deref(), Some("Forecasts"));
    assert_eq!(server.command, "weather-mcp");
    assert_eq!(server.command_kind, CommandKind::Node);
    assert_eq!(server.args, vec!["--verbose".to_string()]);
    assert_eq!(server.env_keys, vec!["API_KEY".to_string()]);
    assert_eq!(server.installed_at, 1_700_000_000);
    assert_eq!(server.last_connected_at, Some(1_700_000_100));
}

#[test]
fn a_row_from_before_the_added_columns_takes_their_defaults() {
    // What every such row *was*: before the transport column there was only the
    // subprocess transport, and before the enabled column every install ran.
    let workspace = tempfile::tempdir().expect("tempdir");
    write_legacy_database(workspace.path());

    let store = Store::open(workspace.path()).expect("the legacy file opens");
    let server = store.get_server("srv-legacy").expect("get");

    // `deployment_url` folded into the transport on the way over: an empty one
    // beside a stdio row was a state that could not mean anything.
    assert_eq!(server.transport, Transport::Stdio);
    assert!(server.enabled);
}

#[test]
fn a_credential_written_before_the_extraction_is_still_readable() {
    // The reason the file is not migrated: these are the values a user pasted
    // in, and re-collecting them means re-authorizing every integration.
    let workspace = tempfile::tempdir().expect("tempdir");
    write_legacy_database(workspace.path());

    let store = Store::open(workspace.path()).expect("the legacy file opens");
    let env = store.load_env_values("srv-legacy").expect("load");

    assert_eq!(env.get("API_KEY").map(String::as_str), Some("sekrit"));
}

#[test]
fn opening_the_same_file_twice_leaves_it_alone() {
    // The migrations run on every open, so they have to be idempotent: a second
    // launch must not fail on a column it added the first time.
    let workspace = tempfile::tempdir().expect("tempdir");
    write_legacy_database(workspace.path());

    drop(Store::open(workspace.path()).expect("the first open"));
    let store = Store::open(workspace.path()).expect("the second open");

    assert_eq!(store.list_servers().expect("list").len(), 1);
}
