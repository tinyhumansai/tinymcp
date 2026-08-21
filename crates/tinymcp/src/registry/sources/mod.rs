//! The upstream catalogs a user browses.
//!
//! Two of them. The official `modelcontextprotocol/registry` is primary;
//! Smithery is a fallback for servers not yet listed there. Both answer in the
//! same shapes — [`RegistryServerSummary`] and [`RegistryServerDetail`] — and
//! stamp their own identifier on every row, so a caller can attribute a result
//! and an install can route its detail lookup back to the right place.
//!
//! # Why Smithery is off unless a key is configured
//!
//! Smithery's servers do not run standalone. They are reached through
//! Smithery's own gateway using the user's account, with per-server credentials
//! configured on Smithery's site. Without a key they cannot be connected at
//! all, so listing thousands of them would fill a user's catalog with rows that
//! look installable and are not. The key is the opt-in.
//!
//! A *detail* lookup still resolves Smithery whether or not a key is set —
//! otherwise an already-installed Smithery server would stop being
//! inspectable the moment the key was removed.
//!
//! # Dispatch is an enum, not a trait object
//!
//! There are two adapters and adding a third is a code change either way. An
//! enum keeps the dispatch visible, avoids boxing every call, and means the
//! compiler tells anyone adding a source about each place they have to handle
//! it.
//!
//! [`RegistryServerSummary`]: tinymcp_bus::RegistryServerSummary
//! [`RegistryServerDetail`]: tinymcp_bus::RegistryServerDetail

mod encode;
mod official;
mod shared;
mod smithery;
mod types;

pub use encode::encode_path_segment;
pub use official::McpOfficialRegistry;
pub use smithery::SmitheryRegistry;
pub use types::{RegistrySource, Registries, SOURCE_MCP_OFFICIAL, SOURCE_SMITHERY};

#[cfg(test)]
mod test;
