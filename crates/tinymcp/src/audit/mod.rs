//! The durable record of every write an MCP tool performed.
//!
//! A user who has let an agent loose on their tools needs to be able to answer
//! "what did it actually do" afterwards. That is what this is for.
//!
//! # Its own file
//!
//! The audit log lives in `mcp_audit/mcp_audit.db`, separate from the installed
//! -server store. Before the extraction it shared a database with the host's
//! own memory tables, which made it unmovable without taking that host's schema
//! with it. A host that needs audit rows and its own data in one transaction
//! should read them out and write them itself; nothing here needs that
//! coupling.
//!
//! # A summary, never the arguments
//!
//! See [`tinymcp_bus::audit`] for why the recorded arguments are a summary. The
//! bound on a recorded error message is applied here, on the way in, because an
//! error string arrives from a remote server and is not length-bounded by
//! anything else.

pub mod store;

pub use store::AuditStore;
