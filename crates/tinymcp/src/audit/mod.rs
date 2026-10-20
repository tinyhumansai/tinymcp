//! The durable record of every write an MCP tool performed.
//!
//! A user who has let an agent loose on their tools needs to be able to answer
//! "what did it actually do" afterwards. That is what this is for.
//!
//! # Its own stream
//!
//! The audit log is the append-only stream `mcp.audit` on the storage ports
//! (see [`crate::storage`]), apart from the installed-server documents. Before
//! the extraction it shared a database with the host's own memory tables, which
//! made it unmovable without taking that host's schema with it. A record's
//! identifier is its stream offset plus one. A host that needs audit rows and
//! its own data in one transaction should read them out and write them itself;
//! nothing here needs that coupling. Importing a legacy `mcp_audit.db` is the
//! host's job.
//!
//! # A summary, never the arguments
//!
//! See [`tinymcp_bus::audit`] for why the recorded arguments are a summary. The
//! bound on a recorded error message is applied here, on the way in, because an
//! error string arrives from a remote server and is not length-bounded by
//! anything else.

pub mod store;

pub use store::AuditStore;

mod query;
pub use query::McpWriteListQueryExt;
#[cfg(test)]
#[path = "query_tests.rs"]
mod query_test;
