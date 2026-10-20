//! What the module declares it needs from storage.
//!
//! The collections and the stream come from [`storage_schema`], so the grant
//! cannot drift from what the module declares. Only the secret names are
//! listed here: a secret is not renamed by a migration step, and a name outside
//! the `mcp/` owner prefix cannot be declared as a schema item.

use serde::Serialize;
use tinystoragedrivers_core::CollectionSpec;
use tinystoragedrivers_core::schema::Item;

use super::{CACHE_KEY_SECRET, ENV_SECRET_PREFIX, schema::storage_schema};

/// Everything the module reads or writes through the storage ports.
///
/// A host that serves storage to this module over the bus uses it as the
/// module's allowlist: the collections (with the indexes they need), the
/// stream, and the secret names. Anything outside it is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageGrant {
    /// Document collections the module uses.
    pub collections: Vec<String>,
    /// Collections that need full-text search.
    pub searchable: Vec<String>,
    /// Stream names (exact) the module appends to and reads.
    pub streams: Vec<String>,
    /// Secret names the module uses, as exact names or `prefix*` patterns.
    pub secrets: Vec<String>,
}

/// The module's storage grant, generated from [`storage_schema`].
#[must_use]
pub fn grant() -> StorageGrant {
    let schema = storage_schema();
    let mut collections = Vec::new();
    let mut searchable = Vec::new();
    let mut streams = Vec::new();
    for item in schema.items {
        match item {
            Item::Collection { name, spec, .. } => {
                if spec.search.is_some() {
                    searchable.push(name.clone());
                }
                collections.push(name);
            }
            Item::Stream { name, .. } => streams.push(name),
            Item::Blobs { .. } | Item::Secrets { .. } => {}
        }
    }
    StorageGrant {
        collections,
        searchable,
        streams,
        secrets: vec![
            format!("{ENV_SECRET_PREFIX}*"),
            CACHE_KEY_SECRET.to_string(),
        ],
    }
}

/// The collections the module declares, with their indexes.
#[must_use]
pub fn collection_specs() -> Vec<CollectionSpec> {
    storage_schema()
        .items
        .into_iter()
        .filter_map(|item| match item {
            Item::Collection { spec, .. } => Some(spec),
            _ => None,
        })
        .collect()
}
