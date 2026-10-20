//! `TinyMCP`'s executable storage schema and persistence helpers.
//!
//! Collection and index vocabulary comes from [`tinymcp_bus::storage`];
//! ownership, scope, versions, and migration callbacks belong to this module.

use tinystoragedrivers_core::schema::{Owner, Schema, ScopeClass};
use tinystoragedrivers_core::{
    CollectionSpec as CoreCollectionSpec, IndexSpec as CoreIndexSpec, Key,
    SearchSpec as CoreSearchSpec, WriteOptions,
};

use super::migrate;

/// The owner every storage name belongs to.
pub const OWNER: &str = "mcp";
/// The body version every collection is declared at.
pub(crate) const SCHEMA_VERSION: u32 = 2;
pub use tinymcp_bus::storage::AUDIT_STREAM;
pub(crate) use tinymcp_bus::storage::{
    REGISTRY_CACHE, REGISTRY_INDEX, REGISTRY_INDEX_STATE, SERVER_NAMES, SERVERS, TOOL_CACHE,
};

/// Everything the module stores, with the steps that bring a v1 database
/// forward.
///
/// All items live in [`ScopeClass::Module`]: the module never names a tenant,
/// the handles it is given carry the scope.
///
/// # Panics
///
/// Never in practice: the owner name is a constant that `Owner::new` accepts,
/// and a unit test pins that.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the owner is the constant `mcp`, which `Owner::new` accepts; a test pins it"
)]
pub fn storage_schema() -> Schema {
    let owner = Owner::new(OWNER).expect("`mcp` is a valid owner name");
    let schema = tinymcp_bus::storage::storage_needs()
        .needs
        .collections
        .iter()
        .fold(Schema::new(owner), |schema, spec| {
            let collection = CoreCollectionSpec {
                name: spec.name.clone(),
                indexes: spec
                    .indexes
                    .iter()
                    .map(|index| CoreIndexSpec {
                        name: index.name.clone(),
                        fields: index.fields.clone(),
                        unique: index.unique,
                    })
                    .collect(),
                ttl_field: spec.ttl_field.clone(),
                search: spec.search.as_ref().map(|search| CoreSearchSpec {
                    fields: search.fields.clone(),
                }),
            };
            schema.collection_with(SCHEMA_VERSION, collection, ScopeClass::Module)
        })
        .stream(AUDIT_STREAM, ScopeClass::Module);
    migrate::steps().into_iter().fold(schema, Schema::step)
}

/// The options every document write carries: the body version.
#[must_use]
pub(crate) fn write_options() -> WriteOptions {
    WriteOptions::new().schema(SCHEMA_VERSION)
}

/// The document id for `parts`: joined and escaped by [`Key::of`], and hashed
/// when it would be too long.
///
/// # Errors
///
/// Returns the storage error for an empty key.
pub(crate) fn doc_id(parts: &[&str]) -> tinystoragedrivers_core::Result<String> {
    Key::of(parts).map(|key| key.as_str().to_owned())
}

#[cfg(test)]
#[path = "schema_tests.rs"]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
