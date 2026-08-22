//! What a host passes the module when it loads it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use tinymcp_bus::McpClientConfig;

/// The module's configuration blob.
///
/// Arrives as JSON in the loader's configuration slot, and an absent one is the
/// empty object — so every field has a default and a host that supplies nothing
/// gets a working module with no servers and an in-memory store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModuleConfig {
    /// Where the store and the audit log live.
    ///
    /// `None` keeps both in memory, which is what a host that only wants the
    /// statically declared servers should pass — there is nothing to persist,
    /// and creating files for it would leave state nobody asked for.
    pub data_dir: Option<PathBuf>,
    /// The servers, credentials, identity, and proxy.
    pub client: McpClientConfig,
}
