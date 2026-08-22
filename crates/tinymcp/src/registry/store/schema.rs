//! The schema, and the additive migrations that bring an older file up to it.

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Creates every table that does not already exist, then applies the additive
/// column migrations.
///
/// # Errors
///
/// Returns [`Error::Store`] when the statements cannot run.
pub(super) fn initialize(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;

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
             );",
        )
        .map_err(|source| Error::store("creating the schema", source))?;

    migrate(connection)
}

/// The columns added after the schema was first cut.
///
/// Each is nullable or carries a default, so an existing row picks up a
/// sensible value without a data migration. `stdio` and `1` are what every
/// pre-existing row *was*: before the transport column there was only the
/// subprocess transport, and before the enabled column every install was live.
const ADDITIVE_COLUMNS: &[(&str, &str)] = &[
    (
        "transport",
        "ALTER TABLE mcp_servers ADD COLUMN transport TEXT NOT NULL DEFAULT 'stdio'",
    ),
    (
        "deployment_url",
        "ALTER TABLE mcp_servers ADD COLUMN deployment_url TEXT",
    ),
    (
        "enabled",
        "ALTER TABLE mcp_servers ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1",
    ),
];

/// Adds any column this file does not have yet.
fn migrate(connection: &Connection) -> Result<()> {
    let existing = columns_of(connection, "mcp_servers")?;

    for (column, statement) in ADDITIVE_COLUMNS {
        if existing.iter().any(|name| name == column) {
            continue;
        }
        add_column(connection, statement, column)?;
    }

    Ok(())
}

/// Runs one `ADD COLUMN`, treating "already exists" as success.
///
/// `SQLite`'s `ADD COLUMN` has no `IF NOT EXISTS`, and the check above is not
/// atomic against another *process* holding the same file. Swallowing exactly
/// the duplicate-column failure is what makes this idempotent: the column
/// existing is the post-condition wanted, and reporting it turns a benign race
/// into a visible error on a page that is working fine.
///
/// Every other failure still propagates.
pub(super) fn add_column(connection: &Connection, statement: &str, column: &str) -> Result<()> {
    match connection.execute(statement, []) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(ref message)))
            if message.contains("duplicate column name") =>
        {
            tracing::debug!(
                column,
                "the column was added concurrently; treating that as success"
            );
            Ok(())
        }
        Err(source) => Err(Error::store(format!("adding the {column} column"), source)),
    }
}

/// The column names on `table`.
pub(super) fn columns_of(connection: &Connection, table: &str) -> Result<Vec<String>> {
    // `PRAGMA table_info` yields (cid, name, type, notnull, dflt_value, pk).
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|source| Error::store("preparing a table-info query", source))?;

    let names = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|source| Error::store("reading table info", source))?
        .collect::<rusqlite::Result<Vec<String>>>()
        .map_err(|source| Error::store("reading table info", source))?;

    Ok(names)
}
