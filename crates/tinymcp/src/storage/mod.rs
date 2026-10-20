//! Where the module keeps its state: the storage ports, never a database.
//!
//! Everything this crate persists goes through three ports from
//! `tinystoragedrivers`: documents, streams, and secrets. The crate links no
//! database client and picks no backend. A host hands over scoped ports or
//! installs a provider that supplies them.
//!
//! The `TinyBus` contract's [`storage_needs`] declaration describes the static
//! names; this crate builds the executable schema and migrations from it.
//! Hosts apply policy to the declaration and bind a grant to the caller.
//!
//! The module never names a tenant. Scope belongs to the handles it receives.

#[cfg(feature = "module")]
mod bus;
#[cfg(test)]
mod failing;
mod grant;
mod migrate;
mod ports;
mod provider;
mod schema;

#[cfg(feature = "module")]
pub use bus::BusProvider;
pub use grant::{StorageGrant, collection_specs, grant};
pub use ports::StoragePorts;
pub use provider::{
    MemoryProvider, StorageProvider, install_provider, installed_provider, provider_installed,
};
#[cfg(test)]
pub(crate) use schema::SCHEMA_VERSION;
pub use schema::{AUDIT_STREAM, OWNER, storage_schema};
pub(crate) use schema::{
    REGISTRY_CACHE, REGISTRY_INDEX, REGISTRY_INDEX_STATE, SERVER_NAMES, SERVERS, TOOL_CACHE,
    doc_id, write_options,
};
pub use tinymcp_bus::storage::{CACHE_KEY_SECRET, ENV_SECRET_PREFIX, storage_needs};

#[cfg(all(test, feature = "module"))]
pub(crate) mod test_host;

#[cfg(all(test, feature = "module"))]
#[path = "bus_tests.rs"]
mod bus_tests;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "grant_tests.rs"]
mod grant_tests;
