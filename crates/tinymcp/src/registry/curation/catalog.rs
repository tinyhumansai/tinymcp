//! The curated list in force: the built-in servers plus the host's runtime
//! entries.
//!
//! A host adds curated servers through
//! [`McpRegistryAuthConfig::curated_servers`]. They are checked once, when the
//! registry dispatcher takes the settings ([`validated_runtime_entries`]), and
//! merged per call into a [`CuratedCatalog`].
//!
//! # Runtime entries are never promoted
//!
//! Only a built-in server nobody replaced is badged official, leads a first
//! search page, or ranks as curated. A runtime entry is listed, searched and
//! described like the rest, but a configuration line must not be able to make
//! an arbitrary endpoint look vetted. An entry naming a built-in server
//! replaces its endpoint and takes the badge away with it.

use std::collections::HashSet;

use serde_json::json;
use tinymcp_bus::{
    CuratedEntryAuth, CuratedEntryTransport, CuratedServerEntry, ExtraFields,
    McpRegistryAuthConfig, RegistryConnection, RegistryServerDetail, RegistryServerSummary,
};

use super::types::{CURATED_SERVERS, CuratedAuth, CuratedServer, CuratedTransport, curated_server};
use crate::registry::sources::SOURCE_MCP_OFFICIAL;

/// The value `auth_kind` takes for a server declaring a static credential.
const AUTH_KIND_API_KEY: &str = "api_key";

/// The built-in curated servers merged with a host's runtime entries.
#[derive(Debug, Clone)]
pub struct CuratedCatalog {
    entries: Vec<CuratedServerEntry>,
    promoted: HashSet<String>,
}

impl CuratedCatalog {
    /// Merges `runtime` (already validated) over the built-in list.
    ///
    /// The built-in servers come first, in their order, minus any a runtime
    /// entry replaces; the runtime entries follow in theirs.
    #[must_use]
    pub fn new(runtime: &[CuratedServerEntry]) -> Self {
        let replaced: HashSet<&str> = runtime
            .iter()
            .map(|entry| entry.qualified_name.as_str())
            .collect();
        let mut entries: Vec<CuratedServerEntry> = CURATED_SERVERS
            .iter()
            .filter(|server| !replaced.contains(server.qualified_name))
            .map(CuratedServer::to_entry)
            .collect();
        let promoted = entries
            .iter()
            .map(|entry| entry.qualified_name.clone())
            .collect();
        entries.extend(runtime.iter().cloned());
        Self { entries, promoted }
    }

    /// The catalog in force for `auth`.
    #[must_use]
    pub fn for_settings(auth: &McpRegistryAuthConfig) -> Self {
        Self::new(&auth.curated_servers)
    }

    /// Every curated entry, built-in first.
    #[must_use]
    pub fn entries(&self) -> &[CuratedServerEntry] {
        &self.entries
    }

    /// The entry for `qualified_name`, by exact match.
    #[must_use]
    pub fn get(&self, qualified_name: &str) -> Option<&CuratedServerEntry> {
        self.entries
            .iter()
            .find(|entry| entry.qualified_name == qualified_name)
    }

    /// Whether `qualified_name` is a built-in server no runtime entry replaced:
    /// the only kind that is badged official and promoted.
    #[must_use]
    pub fn is_promoted(&self, qualified_name: &str) -> bool {
        self.promoted.contains(qualified_name)
    }

    /// The entries [`Self::is_promoted`] accepts, in order.
    pub fn promoted(&self) -> impl Iterator<Item = &CuratedServerEntry> {
        self.entries
            .iter()
            .filter(|entry| self.is_promoted(&entry.qualified_name))
    }

    /// Sets the badge on promoted rows and clears it on every other row, as
    /// [`super::tag_official`] does for the built-in list alone.
    pub fn tag_official(&self, servers: &mut [RegistryServerSummary]) {
        for server in servers.iter_mut() {
            server.official = self.is_promoted(&server.qualified_name);
        }
    }
}

/// The runtime entries worth merging: each checked, the bad ones dropped.
///
/// An entry is dropped, with a warning naming it and the reason, when its
/// `remote_url` or `icon_url` is not `https` with a host and no credentials,
/// its `qualified_name` or `display_name` is blank, or an earlier entry has the
/// same name. The rest load, so one bad line does not cost the whole catalog.
/// An entry naming a built-in server is kept and logged, since it takes the
/// badge away.
#[must_use]
pub fn validated_runtime_entries(entries: Vec<CuratedServerEntry>) -> Vec<CuratedServerEntry> {
    let mut seen = HashSet::new();
    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Err(reason) = check_entry(&entry, &seen) {
            tracing::warn!(
                qualified_name = %entry.qualified_name,
                reason,
                "dropping a runtime curated server"
            );
            continue;
        }
        if let Some(built_in) = curated_server(&entry.qualified_name) {
            tracing::warn!(
                qualified_name = %entry.qualified_name,
                built_in_url = built_in.remote_url,
                runtime_url = %entry.remote_url,
                "a runtime curated server replaces a built-in one and is not badged official"
            );
        }
        seen.insert(entry.qualified_name.clone());
        kept.push(entry);
    }
    kept
}

/// Why `entry` cannot be merged, if it cannot.
fn check_entry(entry: &CuratedServerEntry, seen: &HashSet<String>) -> Result<(), &'static str> {
    if entry.qualified_name.trim().is_empty() {
        return Err("the qualified name is blank");
    }
    if entry.display_name.trim().is_empty() {
        return Err("the display name is blank");
    }
    if seen.contains(&entry.qualified_name) {
        return Err("an earlier runtime entry has the same qualified name");
    }
    if !is_plain_https(&entry.remote_url) {
        return Err("the remote url is not https with a host and no credentials");
    }
    if entry
        .icon_url
        .as_deref()
        .is_some_and(|icon| !is_plain_https(icon))
    {
        return Err("the icon url is not https with a host and no credentials");
    }
    Ok(())
}

/// Whether `raw` is an `https` URL with a host and no embedded credentials.
fn is_plain_https(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some_and(|host| !host.is_empty())
            && url.username().is_empty()
            && url.password().is_none()
    })
}

/// `entry` as a catalog row from the official registry, never badged.
#[must_use]
pub fn entry_summary(entry: &CuratedServerEntry) -> RegistryServerSummary {
    RegistryServerSummary {
        qualified_name: entry.qualified_name.clone(),
        display_name: entry.display_name.clone(),
        description: Some(entry.description.clone()),
        icon_url: entry.icon_url.clone(),
        use_count: 0,
        is_deployed: true,
        source: SOURCE_MCP_OFFICIAL.to_string(),
        official: false,
        website_url: None,
        auth_kind: (entry.auth == CuratedEntryAuth::Token).then(|| AUTH_KIND_API_KEY.to_string()),
        extra: ExtraFields::new(),
    }
}

/// `entry` as a catalog detail record with its one hosted connection.
#[must_use]
pub fn entry_detail(entry: &CuratedServerEntry) -> RegistryServerDetail {
    let config_schema = (entry.auth == CuratedEntryAuth::Token).then(|| {
        json!({
            "properties": { "Authorization": { "x-secret": true } },
            "required": ["Authorization"],
        })
    });
    let connection_type = match entry.transport {
        CuratedEntryTransport::StreamableHttp => CuratedTransport::StreamableHttp,
        CuratedEntryTransport::Sse => CuratedTransport::Sse,
    }
    .connection_type();

    RegistryServerDetail {
        qualified_name: entry.qualified_name.clone(),
        display_name: entry.display_name.clone(),
        description: Some(entry.description.clone()),
        icon_url: entry.icon_url.clone(),
        connections: vec![RegistryConnection {
            r#type: connection_type.to_string(),
            deployment_url: Some(entry.remote_url.clone()),
            config_schema,
            example_config: None,
            published: true,
            extra: ExtraFields::new(),
        }],
        source: SOURCE_MCP_OFFICIAL.to_string(),
        extra: ExtraFields::new(),
    }
}

impl CuratedServer {
    /// This built-in server as a runtime-shaped entry.
    #[must_use]
    pub fn to_entry(&self) -> CuratedServerEntry {
        CuratedServerEntry {
            qualified_name: self.qualified_name.to_string(),
            display_name: self.display_name.to_string(),
            description: self.description.to_string(),
            remote_url: self.remote_url.to_string(),
            transport: match self.transport {
                CuratedTransport::StreamableHttp => CuratedEntryTransport::StreamableHttp,
                CuratedTransport::Sse => CuratedEntryTransport::Sse,
            },
            auth: match self.auth {
                CuratedAuth::Oauth => CuratedEntryAuth::Oauth,
                CuratedAuth::OauthPreregistered => CuratedEntryAuth::OauthPreregistered,
                CuratedAuth::Token => CuratedEntryAuth::Token,
                CuratedAuth::None => CuratedEntryAuth::None,
            },
            icon_url: self.icon_url.map(ToString::to_string),
        }
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
