//! The persistent tool cache: what each server advertised the last time it was
//! listed.
//!
//! A host wants a server's tools *before* the server is reachable — at boot,
//! on a resumed conversation, on a turn that must not wait for a subprocess to
//! spawn. This table is that answer. It is written whenever a listing
//! succeeds and read without touching the network.
//!
//! # A cache, not an authorization
//!
//! A row says what a server offered, not that it may be called. Calling still
//! goes through a live connection, which re-checks everything. That is why a
//! disconnect keeps the row and an uninstall or a disable removes it: the
//! first is a state that will change back, the other two are the user saying
//! the server's tools should stop appearing.
//!
//! # Keyed by a fingerprint of the definition
//!
//! Each row carries a keyed digest of the server definition. The input includes
//! credential values so rotation invalidates cached tools, while the per-store
//! key prevents the database digest from becoming an offline credential oracle.
//! A read whose fingerprint differs misses, so editing a server's definition
//! can never surface the old server's tools under the new one.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tinystoragedrivers_core::Precondition;

use super::types::{Store, now_ms};
use crate::error::Result;
use crate::storage::{TOOL_CACHE, doc_id, write_options};
use tinymcp_bus::{InstalledServer, McpTool};

/// The key prefix for a server pinned in a host's configuration, keeping it
/// out of the installed servers' identifier space.
const STATIC_KEY_PREFIX: &str = "static:";

/// A server's tools as last listed.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedTools {
    /// The tools, as the server advertised them.
    pub tools: Vec<McpTool>,
    /// When they were listed, in Unix epoch milliseconds.
    pub cached_at: i64,
}

/// The cache key for a server pinned in a host's configuration.
#[must_use]
pub fn static_cache_key(name: &str) -> String {
    format!("{STATIC_KEY_PREFIX}{}", name.trim())
}

/// The fingerprint of an installed server's definition.
///
/// Covers connection identity and configuration, including credential values.
/// The raw digest is used only as input to the per-store keyed cache identity;
/// it is never persisted directly.
#[must_use]
pub fn installed_fingerprint(server: &InstalledServer) -> String {
    let mut env_keys = server.env_keys.clone();
    env_keys.sort();
    fingerprint(&[
        server.transport.dispatch_kind(),
        server.transport.deployment_url().unwrap_or_default(),
        server.command_kind.as_str(),
        &server.command,
        &server.args.join("\0"),
        &env_keys.join("\0"),
    ])
}

/// A hex SHA-256 over `parts`, each length-prefixed so no two different part
/// lists can produce the same input.
pub(crate) fn fingerprint(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex(&hasher.finalize())
}

/// A keyed cache identity, so the database does not become an offline oracle
/// for low-entropy credentials that participate in the caller's definition
/// fingerprint.
pub(super) fn keyed_fingerprint(key: &[u8; 32], value: &str) -> String {
    use sha2::{Digest as _, Sha256};

    const BLOCK_BYTES: usize = 64;
    let mut inner_pad = [0x36; BLOCK_BYTES];
    let mut outer_pad = [0x5c; BLOCK_BYTES];
    for (index, byte) in key.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(value.as_bytes());
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    hex(&outer.finalize())
}

/// Lower-case hex of the SHA-256 of `bytes`: a document id that is safe for
/// any key, however long or whatever it contains.
pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Lower-case hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The document id of a server key, as a store error when it cannot be built.
fn key(server_key: &str) -> Result<String> {
    doc_id(&[server_key]).map_err(|source| crate::Error::store("building a document id", source))
}

impl Store {
    /// Records `tools` as what the server under `server_key` advertises now.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Store`] when the write fails.
    pub fn put_cached_tools(
        &self,
        server_key: &str,
        fingerprint: &str,
        tools: &[McpTool],
    ) -> Result<()> {
        self.write_cached_tools(server_key, fingerprint, tools, now_ms())?;
        tracing::debug!(server_key, tools = tools.len(), "cached a tool listing");
        Ok(())
    }

    fn write_cached_tools(
        &self,
        server_key: &str,
        fingerprint: &str,
        tools: &[McpTool],
        cached_at: i64,
    ) -> Result<()> {
        let fingerprint = self.keyed_cache_fingerprint(fingerprint);
        let id = key(server_key)?;
        let doc = serde_json::to_value(ToolCacheDoc {
            server_key: server_key.to_string(),
            fingerprint,
            tools: serde_json::to_value(tools)?,
            cached_ms: cached_at,
        })?;
        self.ports
            .run("writing the tool cache", move |ports| async move {
                ports
                    .documents
                    .put_with(TOOL_CACHE, &id, doc, Precondition::None, write_options())
                    .await
                    .map(|_| ())
            })
    }

    /// The tools last cached for `server_key`, when they were cached for the
    /// same definition.
    ///
    /// A row whose fingerprint differs is treated as absent: it describes a
    /// server that no longer exists under this key.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Store`] when the query fails or the row cannot be
    /// decoded.
    pub fn cached_tools(&self, server_key: &str, fingerprint: &str) -> Result<Option<CachedTools>> {
        let fingerprint = self.keyed_cache_fingerprint(fingerprint);
        let id = key(server_key)?;
        let found = self
            .ports
            .run("reading the tool cache", move |ports| async move {
                ports.documents.get(TOOL_CACHE, &id).await
            })?;
        let Some(found) = found else {
            return Ok(None);
        };
        let row: ToolCacheDoc = serde_json::from_value(found.doc)?;
        if row.fingerprint != fingerprint {
            tracing::debug!(server_key, "the cached tools describe another definition");
            return Ok(None);
        }
        Ok(Some(CachedTools {
            tools: serde_json::from_value(row.tools)?,
            cached_at: row.cached_ms,
        }))
    }

    /// Drops the cached tools for `server_key`, reporting whether a row went.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Store`] when the delete fails.
    pub fn forget_cached_tools(&self, server_key: &str) -> Result<bool> {
        let id = key(server_key)?;
        self.ports
            .run("forgetting cached tools", move |ports| async move {
                ports
                    .documents
                    .delete(TOOL_CACHE, &id, Precondition::None)
                    .await
            })
    }
}

/// One tool-cache document.
#[derive(Debug, Serialize, Deserialize)]
struct ToolCacheDoc {
    server_key: String,
    fingerprint: String,
    tools: serde_json::Value,
    cached_ms: i64,
}
