//! The audit store and its two operations.

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use rusqlite::types::Type;
use rusqlite::{Connection, Row, ToSql, params};
use serde_json::Value;

use crate::error::{Error, Result};
use tinymcp_bus::{ERROR_MESSAGE_MAX_BYTES, McpWriteListQuery, McpWriteRecord, NewMcpWriteRecord};

/// The directory the audit file lives in, under the host's data directory.
const DB_DIR: &str = "mcp_audit";
/// The audit database filename.
const DB_FILE: &str = "mcp_audit.db";

/// Every column of `mcp_writes`, in the order [`map_record`] reads them.
const RECORD_COLUMNS: &str = "id, timestamp_ms, client_info, tool_name, args_summary, \
     resulting_chunk_id, success, error_message";

/// The durable record of MCP tool writes.
#[derive(Debug)]
pub struct AuditStore {
    connection: Mutex<Connection>,
}

impl AuditStore {
    /// Opens, creating the directory and file if they do not exist.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StoreIo`] when the directory cannot be created, and
    /// [`Error::Store`] when the file cannot be opened or the schema created.
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
            Error::store(
                format!("opening the audit log at {}", path.display()),
                source,
            )
        })?;

        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS mcp_writes (
                     id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                     timestamp_ms        INTEGER NOT NULL,
                     client_info         TEXT NOT NULL,
                     tool_name           TEXT NOT NULL,
                     args_summary        TEXT,
                     resulting_chunk_id  TEXT,
                     success             INTEGER NOT NULL,
                     error_message       TEXT
                 );

                 -- The listing orders by time and filters by client or tool.
                 CREATE INDEX IF NOT EXISTS mcp_writes_timestamp
                     ON mcp_writes (timestamp_ms DESC, id DESC);
                 CREATE INDEX IF NOT EXISTS mcp_writes_client
                     ON mcp_writes (client_info);
                 CREATE INDEX IF NOT EXISTS mcp_writes_tool
                     ON mcp_writes (tool_name);",
            )
            .map_err(|source| Error::store("creating the audit schema", source))?;

        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Opens an in-memory audit log, for tests.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the schema cannot be created.
    pub fn open_in_memory() -> Result<Self> {
        Self::open_file(Path::new(":memory:"))
    }

    /// The path an audit log for `data_dir` would use.
    #[must_use]
    pub fn path_for(data_dir: &Path) -> PathBuf {
        data_dir.join(DB_DIR).join(DB_FILE)
    }

    /// Records one write, returning its assigned row identifier.
    ///
    /// The error message is truncated to [`ERROR_MESSAGE_MAX_BYTES`] on a
    /// character boundary. It comes from a remote server and is bounded by
    /// nothing else, and an audit row that can be made arbitrarily large by a
    /// misbehaving server is a way to fill a user's disk.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Serialization`] when the argument summary cannot be
    /// encoded, and [`Error::Store`] when the insert fails.
    pub fn record(&self, record: &NewMcpWriteRecord) -> Result<i64> {
        let args_summary = serde_json::to_string(&record.args_summary)?;
        let error_message = truncate_error(record.error_message.as_deref());

        let connection = self.connection.lock();
        connection
            .execute(
                "INSERT INTO mcp_writes
                     (timestamp_ms, client_info, tool_name, args_summary,
                      resulting_chunk_id, success, error_message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    record.timestamp_ms,
                    record.client_info,
                    record.tool_name,
                    args_summary,
                    record.resulting_chunk_id,
                    i64::from(record.success),
                    error_message,
                ],
            )
            .map_err(|source| Error::store("recording a write", source))?;

        Ok(connection.last_insert_rowid())
    }

    /// Lists recorded writes, most recent first.
    ///
    /// The query's bounds are applied through its `resolved_*` accessors, so a
    /// caller cannot ask for an unbounded page.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails, and
    /// [`Error::MalformedResponse`] when a bound would not fit the column type.
    pub fn list(&self, query: &McpWriteListQuery) -> Result<Vec<McpWriteRecord>> {
        let mut sql = format!("SELECT {RECORD_COLUMNS} FROM mcp_writes WHERE 1=1");
        let mut bound: Vec<Box<dyn ToSql>> = Vec::new();

        if let Some(since_ms) = query.since_ms {
            sql.push_str(" AND timestamp_ms >= ?");
            bound.push(Box::new(to_sqlite_integer(since_ms, "since_ms")?));
        }
        if let Some(client) = query.resolved_client_filter() {
            sql.push_str(" AND client_info = ?");
            bound.push(Box::new(client.to_string()));
        }
        if let Some(tool) = query.resolved_tool_filter() {
            sql.push_str(" AND tool_name = ?");
            bound.push(Box::new(tool.to_string()));
        }
        if query.resolved_success_only() {
            sql.push_str(" AND success = 1");
        }

        // The identifier breaks ties so two writes in the same millisecond do
        // not swap places between two reads of the same page.
        sql.push_str(" ORDER BY timestamp_ms DESC, id DESC LIMIT ? OFFSET ?");
        bound.push(Box::new(to_sqlite_integer(query.resolved_limit(), "limit")?));
        bound.push(Box::new(to_sqlite_integer(
            query.resolved_offset(),
            "offset",
        )?));

        let connection = self.connection.lock();
        let mut statement = connection
            .prepare(&sql)
            .map_err(|source| Error::store("preparing an audit listing", source))?;

        let parameters: Vec<&dyn ToSql> = bound.iter().map(AsRef::as_ref).collect();
        let records = statement
            .query_map(parameters.as_slice(), map_record)
            .map_err(|source| Error::store("listing audit records", source))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| Error::store("reading an audit row", source))?;

        Ok(records)
    }
}

/// Truncates an error message on a character boundary.
fn truncate_error(message: Option<&str>) -> Option<String> {
    let message = message?;
    if message.len() <= ERROR_MESSAGE_MAX_BYTES {
        return Some(message.to_string());
    }

    let mut end = ERROR_MESSAGE_MAX_BYTES;
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    Some(message.get(..end).unwrap_or_default().to_string())
}

/// Narrows an unsigned bound to what the column can hold.
fn to_sqlite_integer(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::malformed(format!("{field} is too large for a sqlite integer")))
}

/// Reads one `mcp_writes` row.
fn map_record(row: &Row<'_>) -> rusqlite::Result<McpWriteRecord> {
    let args_summary = match row.get::<_, Option<String>>(4)? {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|error| rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(error)))?,
        None => Value::Null,
    };
    let success: i64 = row.get(6)?;

    Ok(McpWriteRecord {
        id: row.get(0)?,
        timestamp_ms: row.get(1)?,
        client_info: row.get(2)?,
        tool_name: row.get(3)?,
        args_summary,
        resulting_chunk_id: row.get(5)?,
        success: success != 0,
        error_message: row.get(7)?,
    })
}
