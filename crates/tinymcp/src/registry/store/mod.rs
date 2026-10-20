//! Persistence for installed servers, their credentials, and the browse cache.
//!
//! Written against the storage ports (see [`crate::storage`]); the module
//! links no database. What goes where:
//!
//! | Port | Name | Holds |
//! | --- | --- | --- |
//! | documents | `mcp.servers` | one document per installed server, with no credential values |
//! | documents | `mcp.server_names` | one claim per qualified name, so installing a service twice is one install |
//! | documents | `mcp.registry_cache` | upstream browse responses, with a timestamp |
//! | documents | `mcp.tool_cache` | each server's last advertised tool list, keyed by a definition fingerprint |
//! | documents | `mcp.registry_index` | the local copy of the official catalog, one document per server, searched with `DocumentStore::search` |
//! | documents | `mcp.registry_index_state` | where that copy's sync stands |
//! | secrets | `mcp_client_env/<server id>` | the server's environment values and OAuth bundle, one secret |
//! | secrets | `mcp_cache_fingerprint_key` | the key the tool-cache fingerprints are keyed with |
//!
//! Tool-cache definition fingerprints are keyed before they are stored, with a
//! key held in the secret store, so a copy of the documents alone cannot be
//! used to test guesses for configured secrets.
//!
//! # Credentials live in the secret store for a reason
//!
//! [`InstalledServer`] carries the *names* of a server's environment variables
//! and never their values. Install records are listed over the bus, rendered in
//! user interfaces, and written to logs; the values are read only when a server
//! is about to be spawned. Keeping them in the secret store, encrypted by the
//! host's driver and unreachable by a document query, means neither the type
//! that travels nor a backend listing can carry them even by accident.
//!
//! # Legacy files
//!
//! Earlier releases kept all of this in `mcp_clients/mcp_clients.db`. The
//! module no longer reads it; importing an existing file belongs to the host,
//! which reads it read-only and writes the records through these ports.
//!
//! [`InstalledServer`]: tinymcp_bus::InstalledServer

pub(crate) mod index;
#[cfg(test)]
mod testing;
mod tool_cache;
mod types;

pub use tool_cache::{CachedTools, installed_fingerprint, static_cache_key};
pub(crate) use tool_cache::{fingerprint, hex_digest};
pub use types::Store;
pub(crate) use types::now_ms;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
