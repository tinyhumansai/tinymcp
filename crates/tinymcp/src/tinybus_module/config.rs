//! What a host passes the module when it loads it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use tinymcp_bus::McpClientConfig;

/// The module's configuration blob.
///
/// Arrives as JSON in the loader's configuration slot. A host that configures
/// nothing supplies either the empty object or `null`, so both decode to the
/// same thing: a working module with no servers and an in-memory store.
///
/// `#[serde(default)]` covers the empty object, because it fills in absent
/// *fields*. It does not cover `null`, which is a whole document of the wrong
/// type — hence the hand-written [`Deserialize`] below. A module that refused
/// `null` would fail to load for exactly the host that asked nothing of it.
#[derive(Debug, Clone, Default, Serialize)]
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

/// The fields as they appear on the wire.
///
/// Separate from [`ModuleConfig`] so the hand-written deserializer below can
/// derive the field handling rather than restate it.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Wire {
    data_dir: Option<PathBuf>,
    client: McpClientConfig,
}

impl<'de> Deserialize<'de> for ModuleConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // `Option` is what turns `null` into "nothing configured" rather than a
        // type error. Everything else decodes through the derived impl.
        let wire = Option::<Wire>::deserialize(deserializer)?.unwrap_or_default();

        Ok(Self {
            data_dir: wire.data_dir,
            client: wire.client,
        })
    }
}
