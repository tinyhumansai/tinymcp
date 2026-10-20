//! `TinyMCP`'s storage needs for host policy evaluation.

use std::sync::LazyLock;

use tinystoragedrivers_bus::{
    CollectionSpec, IndexSpec, StorageNeeds, StorageNeedsWithExactSecrets,
};

/// One document per installed server.
pub const SERVERS: &str = "mcp.servers";
/// Claims on qualified names, so installing one service twice is one install.
pub const SERVER_NAMES: &str = "mcp.server_names";
/// Upstream browse responses, with a timestamp.
pub const REGISTRY_CACHE: &str = "mcp.registry_cache";
/// Each server's last advertised tool list, keyed by a definition fingerprint.
pub const TOOL_CACHE: &str = "mcp.tool_cache";
/// The local copy of an upstream catalog, one document per server.
pub const REGISTRY_INDEX: &str = "mcp.registry_index";
/// Where each catalog's sync stands.
pub const REGISTRY_INDEX_STATE: &str = "mcp.registry_index_state";
/// The append-only stream of tool-write audit records.
pub const AUDIT_STREAM: &str = "mcp.audit";
/// Prefix for each server's environment and OAuth bundle secret.
pub const ENV_SECRET_PREFIX: &str = "mcp_client_env/";
/// Exact secret name for the tool-cache fingerprint key.
pub const CACHE_KEY_SECRET: &str = "mcp_cache_fingerprint_key";

const STREAM_PREFIXES: &[&str] = &[AUDIT_STREAM];
const SECRET_PREFIXES: &[&str] = &[ENV_SECRET_PREFIX];
const EXACT_SECRET_NAMES: &[&str] = &[CACHE_KEY_SECRET];

static COLLECTIONS: LazyLock<Vec<CollectionSpec>> = LazyLock::new(|| {
    vec![
        collection(
            SERVERS,
            &[
                ("qualified_name", "qualified_name"),
                ("installed_ms", "installed_ms"),
            ],
        ),
        collection(SERVER_NAMES, &[]),
        collection(REGISTRY_CACHE, &[("cache_key", "cache_key")]),
        collection(TOOL_CACHE, &[]),
        collection(REGISTRY_INDEX, &[("source", "source")]),
        collection(REGISTRY_INDEX_STATE, &[]),
    ]
});

fn collection(name: &str, indexes: &[(&str, &str)]) -> CollectionSpec {
    CollectionSpec {
        name: name.to_owned(),
        indexes: indexes
            .iter()
            .map(|(name, field)| IndexSpec {
                name: (*name).to_owned(),
                fields: vec![(*field).to_owned()],
                unique: false,
            })
            .collect(),
        ttl_field: None,
        search: None,
    }
}

/// Returns `TinyMCP`'s canonical storage needs for host policy evaluation.
///
/// Collection declarations are process-static. The cache key is an exact
/// secret need; only server environment bundles use a prefix.
#[must_use]
pub fn storage_needs() -> StorageNeedsWithExactSecrets<'static> {
    StorageNeeds {
        collections: COLLECTIONS.as_slice(),
        blob_prefixes: &[],
        stream_prefixes: STREAM_PREFIXES,
        secret_prefixes: SECRET_PREFIXES,
    }
    .with_exact_secret_names(EXACT_SECRET_NAMES)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
