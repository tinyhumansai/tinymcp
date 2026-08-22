//! The write-audit log's vocabulary: what gets recorded, and how it is queried
//! back.
//!
//! Every MCP tool call that writes something is recorded, so a user can answer
//! "what did this agent do, and when" after the fact.
//!
//! # A summary, not the arguments
//!
//! [`NewMcpWriteRecord::args_summary`] is a summary on purpose. Audit rows are
//! read by people, retained indefinitely, and exported; tool arguments
//! routinely carry credentials, file contents, and personal data. Recording the
//! arguments verbatim would turn an accountability feature into a durable copy
//! of everything sensitive that ever passed through a tool.
//!
//! # The bounds are the design
//!
//! Three constants — [`DEFAULT_LIST_LIMIT`], [`MAX_LIST_LIMIT`], and
//! [`ERROR_MESSAGE_MAX_BYTES`] — exist because the two inputs here are
//! unbounded. The table grows without limit, so an unclamped page size turns a
//! listing into an out-of-memory condition; error text arrives from a remote
//! server, so an untruncated message turns one bad response into an
//! arbitrarily large row.
//!
//! [`McpWriteListQuery`] carries `resolved_*` accessors that apply those
//! bounds, along with the rule that a blank filter means "no filter" rather
//! than "match nothing". They live on the query rather than in the store
//! because they are part of what the query *means*, and a second caller
//! re-deriving them is a second chance to get them wrong.

mod types;

pub use types::{
    DEFAULT_LIST_LIMIT, ERROR_MESSAGE_MAX_BYTES, MAX_LIST_LIMIT, McpWriteListQuery, McpWriteRecord,
    NewMcpWriteRecord,
};

#[cfg(test)]
mod test;
