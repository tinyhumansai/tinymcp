//! Write-audit payload types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The default page size for a write listing.
pub const DEFAULT_LIST_LIMIT: u64 = 50;

/// The largest page a write listing will return.
///
/// A caller asking for more gets this many. The audit table grows without
/// bound, so an unbounded query is a way to turn a listing into an
/// out-of-memory condition.
pub const MAX_LIST_LIMIT: u64 = 500;

/// The cap applied to a recorded error message, in bytes.
///
/// Error text comes from a remote server and is not length-bounded by
/// anything; the audit row is.
pub const ERROR_MESSAGE_MAX_BYTES: usize = 1024;

/// A write to be recorded in the audit log.
///
/// This is the pre-insert shape: no identifier, because the store assigns one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewMcpWriteRecord {
    /// When the write happened, in Unix epoch milliseconds.
    pub timestamp_ms: i64,
    /// Which client performed it.
    pub client_info: String,
    /// Which tool was called.
    pub tool_name: String,
    /// A summary of the arguments.
    ///
    /// A *summary*, deliberately, not the arguments. Audit rows are read by
    /// people and retained indefinitely, and tool arguments routinely carry
    /// credentials, file contents, and personal data that has no business
    /// sitting in a log forever.
    pub args_summary: Value,
    /// The identifier of whatever the write produced, when it produced one.
    #[serde(default)]
    pub resulting_chunk_id: Option<String>,
    /// Whether the write succeeded.
    pub success: bool,
    /// Why it failed, when it failed.
    ///
    /// Truncated to [`ERROR_MESSAGE_MAX_BYTES`] on the way in.
    #[serde(default)]
    pub error_message: Option<String>,
}

/// A recorded write, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpWriteRecord {
    /// The store-assigned row identifier.
    pub id: i64,
    /// When the write happened, in Unix epoch milliseconds.
    pub timestamp_ms: i64,
    /// Which client performed it.
    pub client_info: String,
    /// Which tool was called.
    pub tool_name: String,
    /// A summary of the arguments. See [`NewMcpWriteRecord::args_summary`].
    pub args_summary: Value,
    /// The identifier of whatever the write produced.
    #[serde(default)]
    pub resulting_chunk_id: Option<String>,
    /// Whether the write succeeded.
    pub success: bool,
    /// Why it failed, when it failed.
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Which recorded writes to return.
///
/// Every field is optional; the empty query is the most recent
/// [`DEFAULT_LIST_LIMIT`] writes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpWriteListQuery {
    /// How many rows to return, clamped to [`MAX_LIST_LIMIT`].
    #[serde(default)]
    pub limit: Option<u64>,
    /// How many rows to skip.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Return only writes at or after this Unix epoch millisecond.
    #[serde(default)]
    pub since_ms: Option<u64>,
    /// Return only writes from this client.
    #[serde(default)]
    pub client_filter: Option<String>,
    /// Return only writes of this tool.
    #[serde(default)]
    pub tool_filter: Option<String>,
    /// When `Some(true)`, return only successful writes.
    ///
    /// `Some(false)` and `None` both mean "no filter" — this mirrors the
    /// predicate the store applies, which only ever adds a `success = 1` clause.
    #[serde(default)]
    pub success_only: Option<bool>,
}

impl McpWriteListQuery {
    /// The page size this query resolves to, clamped to [`MAX_LIST_LIMIT`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp_bus::{McpWriteListQuery, MAX_LIST_LIMIT, DEFAULT_LIST_LIMIT};
    /// assert_eq!(McpWriteListQuery::default().resolved_limit(), DEFAULT_LIST_LIMIT);
    ///
    /// let mut query = McpWriteListQuery::default();
    /// query.limit = Some(10_000);
    /// assert_eq!(query.resolved_limit(), MAX_LIST_LIMIT);
    /// ```
    #[must_use]
    pub fn resolved_limit(&self) -> u64 {
        self.limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST_LIMIT)
    }

    /// The offset this query resolves to.
    #[must_use]
    pub fn resolved_offset(&self) -> u64 {
        self.offset.unwrap_or(0)
    }
}
