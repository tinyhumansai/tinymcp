//! The store and its operations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde_json::Value;

use crate::error::{Error, Result};
use tinymcp_bus::{CommandKind, InstalledServer, Transport};

/// The directory the database file lives in, under the host's data directory.
const DB_DIR: &str = "mcp_clients";
/// The database filename. Unchanged from before the extraction, so an existing
/// file is found rather than orphaned.
const DB_FILE: &str = "mcp_clients.db";

/// How long a cached browse response stays fresh.
const CACHE_TTL_MS: i64 = 10 * 60 * 1_000;

/// Every column of `mcp_servers`, in the order [`map_server`] reads them.
const SERVER_COLUMNS: &str = "server_id, qualified_name, display_name, description, icon_url, \
     command_kind, command, args_json, env_keys_json, config_json, \
     installed_at, last_connected_at, transport, deployment_url, enabled";

/// Persistence for installed servers, their credentials, and the browse cache.
///
/// Holds one connection for its lifetime. See the module documentation for why.
#[derive(Debug)]
pub struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    /// Opens, creating the directory and file if they do not exist.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the directory cannot be created, the file
    /// cannot be opened, or the schema cannot be brought up to date.
    pub fn open(data_dir: &Path) -> Result<Self> {
        let directory = data_dir.join(DB_DIR);
        std::fs::create_dir_all(&directory).map_err(|source| Error::StoreIo {
            path: directory.clone(),
            source: Box::new(source),
        })?;

        Self::open_file(&directory.join(DB_FILE))
    }

    /// Opens one specific file.
    ///
    /// # Errors
    ///
    /// As [`Self::open`], minus the directory creation.
    pub fn open_file(path: &Path) -> Result<Self> {
        let connection = Connection::open(path).map_err(|source| {
            Error::store(format!("opening the store at {}", path.display()), source)
        })?;

        super::schema::initialize(&connection)?;

        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Opens an in-memory store, for tests and for a host that wants no file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the schema cannot be created.
    pub fn open_in_memory() -> Result<Self> {
        Self::open_file(Path::new(":memory:"))
    }

    /// The path a store for `data_dir` would use.
    #[must_use]
    pub fn path_for(data_dir: &Path) -> PathBuf {
        data_dir.join(DB_DIR).join(DB_FILE)
    }

    // -- installed servers --------------------------------------------------

    /// Inserts a server row.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the insert fails, including when the
    /// identifier is already taken.
    pub fn insert_server(&self, server: &InstalledServer) -> Result<()> {
        let columns = ServerColumns::encode(server)?;
        self.connection
            .lock()
            .execute(
                &format!(
                    "INSERT INTO mcp_servers ({SERVER_COLUMNS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
                ),
                columns.as_params(server).as_slice(),
            )
            .map_err(|source| Error::store("inserting a server", source))?;
        Ok(())
    }

    /// Inserts a server only if no row already carries its qualified name.
    ///
    /// Returns whether this call inserted it.
    ///
    /// # Why this is one statement
    ///
    /// The install flow reads for an existing install before writing, and an
    /// awaited registry lookup sits between that read and the write. The
    /// primary key is the install identifier, not the qualified name, so two
    /// concurrent installs of the same service could both miss and both insert.
    /// `INSERT … SELECT … WHERE NOT EXISTS` closes that window without a schema
    /// change.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the statement fails.
    pub fn insert_server_if_absent(&self, server: &InstalledServer) -> Result<bool> {
        let columns = ServerColumns::encode(server)?;
        let inserted = self
            .connection
            .lock()
            .execute(
                &format!(
                    "INSERT INTO mcp_servers ({SERVER_COLUMNS})
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
                     WHERE NOT EXISTS (SELECT 1 FROM mcp_servers WHERE qualified_name = ?2)"
                ),
                columns.as_params(server).as_slice(),
            )
            .map_err(|source| Error::store("inserting a server if absent", source))?;
        Ok(inserted > 0)
    }

    /// Every installed server, oldest install first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails or a row cannot be read.
    pub fn list_servers(&self) -> Result<Vec<InstalledServer>> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare(&format!(
                "SELECT {SERVER_COLUMNS} FROM mcp_servers ORDER BY installed_at ASC"
            ))
            .map_err(|source| Error::store("preparing a server listing", source))?;

        let servers = statement
            .query_map([], map_server)
            .map_err(|source| Error::store("listing servers", source))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| Error::store("reading a server row", source))?;

        Ok(servers)
    }

    /// One server by its install identifier.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when there is no such install, and
    /// [`Error::Store`] when the query fails.
    pub fn get_server(&self, server_id: &str) -> Result<InstalledServer> {
        self.find_server(server_id)?
            .ok_or_else(|| Error::UnknownServer {
                server: server_id.to_string(),
            })
    }

    /// One server by its install identifier, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails.
    pub fn find_server(&self, server_id: &str) -> Result<Option<InstalledServer>> {
        let connection = self.connection.lock();
        connection
            .query_row(
                &format!("SELECT {SERVER_COLUMNS} FROM mcp_servers WHERE server_id = ?1"),
                params![server_id],
                map_server,
            )
            .optional()
            .map_err(|source| Error::store("looking a server up", source))
    }

    /// The earliest install carrying `qualified_name`, if any.
    ///
    /// The schema permits several installs of one service — the primary key is
    /// the install identifier — so this returns the oldest, which is what keeps
    /// installing idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails.
    pub fn find_server_by_qualified_name(
        &self,
        qualified_name: &str,
    ) -> Result<Option<InstalledServer>> {
        let connection = self.connection.lock();
        connection
            .query_row(
                &format!(
                    "SELECT {SERVER_COLUMNS} FROM mcp_servers WHERE qualified_name = ?1
                     ORDER BY installed_at ASC LIMIT 1"
                ),
                params![qualified_name],
                map_server,
            )
            .optional()
            .map_err(|source| Error::store("looking a server up by qualified name", source))
    }

    /// Removes a server, its credentials, and returns whether a row went.
    ///
    /// The credential rows go with it through the foreign key's cascade.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the delete fails.
    pub fn delete_server(&self, server_id: &str) -> Result<bool> {
        let removed = self
            .connection
            .lock()
            .execute(
                "DELETE FROM mcp_servers WHERE server_id = ?1",
                params![server_id],
            )
            .map_err(|source| Error::store("deleting a server", source))?;
        Ok(removed > 0)
    }

    /// Replaces a server's recorded environment variable *names*.
    ///
    /// The values live in their own table; this is the list shown in a listing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the update fails.
    pub fn update_env_keys(&self, server_id: &str, env_keys: &[String]) -> Result<()> {
        let encoded = serde_json::to_string(env_keys)?;
        self.connection
            .lock()
            .execute(
                "UPDATE mcp_servers SET env_keys_json = ?2 WHERE server_id = ?1",
                params![server_id, encoded],
            )
            .map_err(|source| Error::store("updating a server's environment keys", source))?;
        Ok(())
    }

    /// Replaces a server's stored configuration blob. `None` clears it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the update fails.
    pub fn update_config(&self, server_id: &str, config: Option<&Value>) -> Result<()> {
        let encoded = config.map(serde_json::to_string).transpose()?;
        self.connection
            .lock()
            .execute(
                "UPDATE mcp_servers SET config_json = ?2 WHERE server_id = ?1",
                params![server_id, encoded],
            )
            .map_err(|source| Error::store("updating a server's configuration", source))?;
        Ok(())
    }

    /// Turns a server on or off without uninstalling it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the update fails.
    pub fn update_enabled(&self, server_id: &str, enabled: bool) -> Result<()> {
        self.connection
            .lock()
            .execute(
                "UPDATE mcp_servers SET enabled = ?2 WHERE server_id = ?1",
                params![server_id, i64::from(enabled)],
            )
            .map_err(|source| Error::store("updating a server's enabled flag", source))?;
        Ok(())
    }

    /// Records that a server connected successfully, just now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the update fails.
    pub fn touch_last_connected(&self, server_id: &str) -> Result<()> {
        self.connection
            .lock()
            .execute(
                "UPDATE mcp_servers SET last_connected_at = ?2 WHERE server_id = ?1",
                params![server_id, now_ms()],
            )
            .map_err(|source| Error::store("recording a successful connection", source))?;
        Ok(())
    }

    // -- credentials --------------------------------------------------------

    /// Replaces every stored credential for a server.
    ///
    /// Existing rows are cleared first, so a name removed from `env` stops
    /// being sent to the server rather than lingering.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when a statement fails.
    pub fn set_env_values(&self, server_id: &str, env: &BTreeMap<String, String>) -> Result<()> {
        let mut connection = self.connection.lock();
        let transaction = connection
            .transaction()
            .map_err(|source| Error::store("beginning a credential update", source))?;

        transaction
            .execute(
                "DELETE FROM mcp_client_env WHERE server_id = ?1",
                params![server_id],
            )
            .map_err(|source| Error::store("clearing previous credentials", source))?;

        for (key, value) in env {
            transaction
                .execute(
                    "INSERT INTO mcp_client_env (server_id, key, value) VALUES (?1, ?2, ?3)",
                    params![server_id, key, value],
                )
                .map_err(|source| Error::store("storing a credential", source))?;
        }

        transaction
            .commit()
            .map_err(|source| Error::store("committing a credential update", source))
    }

    /// Loads a server's credentials, for spawning it.
    ///
    /// The values returned here must never be serialized into a response or
    /// written to a log.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails.
    pub fn load_env_values(&self, server_id: &str) -> Result<BTreeMap<String, String>> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare("SELECT key, value FROM mcp_client_env WHERE server_id = ?1")
            .map_err(|source| Error::store("preparing a credential query", source))?;

        let pairs = statement
            .query_map(params![server_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|source| Error::store("loading credentials", source))?
            .collect::<rusqlite::Result<BTreeMap<_, _>>>()
            .map_err(|source| Error::store("reading a credential row", source))?;

        Ok(pairs)
    }

    // -- browse cache -------------------------------------------------------

    /// A cached browse response, if one is present and still fresh.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails.
    pub fn cached(&self, cache_key: &str) -> Result<Option<String>> {
        let connection = self.connection.lock();
        let row: Option<(String, i64)> = connection
            .query_row(
                "SELECT body_json, cached_at FROM mcp_registry_cache WHERE cache_key = ?1",
                params![cache_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|source| Error::store("reading the browse cache", source))?;

        Ok(match row {
            Some((body, cached_at)) if now_ms() - cached_at < CACHE_TTL_MS => Some(body),
            _ => None,
        })
    }

    /// Caches a browse response against the current time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    pub fn cache(&self, cache_key: &str, body_json: &str) -> Result<()> {
        self.connection
            .lock()
            .execute(
                "INSERT OR REPLACE INTO mcp_registry_cache (cache_key, body_json, cached_at)
                 VALUES (?1, ?2, ?3)",
                params![cache_key, body_json, now_ms()],
            )
            .map_err(|source| Error::store("writing to the browse cache", source))?;
        Ok(())
    }

    /// Runs `body` against the connection. For tests that need raw access.
    #[cfg(test)]
    pub(super) fn with_connection<T>(&self, body: impl FnOnce(&Connection) -> T) -> T {
        body(&self.connection.lock())
    }
}

/// The JSON-encoded columns of a server row, held so they outlive the
/// parameter slice that borrows them.
struct ServerColumns {
    args: String,
    env_keys: String,
    config: Option<String>,
}

impl ServerColumns {
    /// Encodes the three JSON columns.
    fn encode(server: &InstalledServer) -> Result<Self> {
        Ok(Self {
            args: serde_json::to_string(&server.args)?,
            env_keys: serde_json::to_string(&server.env_keys)?,
            config: server.config.as_ref().map(serde_json::to_string).transpose()?,
        })
    }

    /// The full parameter list, in `SERVER_COLUMNS` order.
    fn as_params<'a>(&'a self, server: &'a InstalledServer) -> Vec<&'a dyn rusqlite::ToSql> {
        vec![
            &server.server_id,
            &server.qualified_name,
            &server.display_name,
            &server.description,
            &server.icon_url,
            &CommandKindSql(server.command_kind),
            &server.command,
            &self.args,
            &self.env_keys,
            &self.config,
            &server.installed_at,
            &server.last_connected_at,
            &TransportKindSql(&server.transport),
            &DeploymentUrlSql(&server.transport),
            &EnabledSql(server.enabled),
        ]
    }
}

/// Binds a [`CommandKind`] as its stable string.
struct CommandKindSql(CommandKind);

impl rusqlite::ToSql for CommandKindSql {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.as_str().to_sql()
    }
}

/// Binds a [`Transport`] as its stable dispatch string.
struct TransportKindSql<'a>(&'a Transport);

impl rusqlite::ToSql for TransportKindSql<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.dispatch_kind().to_sql()
    }
}

/// Binds a [`Transport`]'s endpoint, or null for a subprocess.
struct DeploymentUrlSql<'a>(&'a Transport);

impl rusqlite::ToSql for DeploymentUrlSql<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.deployment_url().to_sql()
    }
}

/// Binds a flag as SQLite's integer boolean.
struct EnabledSql(bool);

impl rusqlite::ToSql for EnabledSql {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        i64::from(self.0).to_sql()
    }
}

/// Reads one `mcp_servers` row.
///
/// The three post-migration columns are read as optional and defaulted, so a
/// file that somehow escaped the migration still loads its rows rather than
/// failing the whole listing. `stdio` and enabled are what those rows *were*
/// before the columns existed.
fn map_server(row: &Row<'_>) -> rusqlite::Result<InstalledServer> {
    let args = decode_column(row, 7)?;
    let env_keys = decode_column(row, 8)?;
    let config = row
        .get::<_, Option<String>>(9)?
        .map(|raw| {
            serde_json::from_str(&raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()?;

    let transport_kind: String = row.get::<_, Option<String>>(12)?.unwrap_or_default();
    let deployment_url: Option<String> = row.get(13)?;
    let enabled: i64 = row.get::<_, Option<i64>>(14)?.unwrap_or(1);

    Ok(InstalledServer {
        server_id: row.get(0)?,
        qualified_name: row.get(1)?,
        display_name: row.get(2)?,
        description: row.get(3)?,
        icon_url: row.get(4)?,
        command_kind: CommandKind::parse(&row.get::<_, String>(5)?),
        command: row.get(6)?,
        args,
        env_keys,
        config,
        installed_at: row.get(10)?,
        last_connected_at: row.get(11)?,
        transport: Transport::parse(&transport_kind, deployment_url.as_deref()),
        enabled: enabled != 0,
    })
}

/// Reads a JSON-encoded string column into a value.
fn decode_column<T: serde::de::DeserializeOwned>(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

/// The current time in Unix epoch milliseconds.
///
/// A clock set before the epoch reads as zero rather than failing. Nothing here
/// makes a decision that a wrong timestamp could make unsafe: the worst case is
/// a cache entry that looks stale.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}
