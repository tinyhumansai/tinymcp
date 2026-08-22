//! Persistence for installed servers, their credentials, and the browse cache.
//!
//! Three tables in one `SQLite` file, `mcp_clients/mcp_clients.db` under the data
//! directory the host supplies:
//!
//! | Table | Holds |
//! | --- | --- |
//! | `mcp_servers` | one row per installed server, with no credential values |
//! | `mcp_client_env` | the credential values, keyed by server and name |
//! | `mcp_registry_cache` | upstream browse responses, with a timestamp |
//!
//! The filename and schema are unchanged from the code this was extracted
//! from, so a user upgrading across the move keeps every server they installed.
//!
//! # Credentials live in their own table for a reason
//!
//! [`InstalledServer`] carries the *names* of a server's environment variables
//! and never their values. Install records are listed over the bus, rendered in
//! user interfaces, and written to logs; the values are read only when a server
//! is about to be spawned. Keeping them in a separate table means the type that
//! travels cannot carry them even by accident.
//!
//! # One connection, held
//!
//! The store owns its connection rather than opening one per call. That is
//! faster, and it closes a race: the schema migration below is a
//! check-then-alter, and the code this replaced opened a fresh connection for
//! every operation, so several concurrent calls could each see a column as
//! missing and then all race to add it. Every loser failed with "duplicate
//! column name", which surfaced to users as a red banner on a page that was
//! otherwise working.
//!
//! [`InstalledServer`]: tinymcp_bus::InstalledServer

pub(crate) mod schema;
mod types;

pub use types::Store;

#[cfg(test)]
mod test;
