//! The dispatcher over the upstream catalogs.

use std::collections::HashMap;

use parking_lot::Mutex;

use super::official::McpOfficialRegistry;
use super::smithery::SmitheryRegistry;
use crate::error::{Error, Result};
use crate::registry::Store;
use tinymcp_bus::{McpRegistryAuthConfig, RegistryServerDetail, RegistryServerSummary};

/// The identifier Smithery stamps on its rows.
pub const SOURCE_SMITHERY: &str = "smithery";

/// The identifier the official registry stamps on its rows.
pub const SOURCE_MCP_OFFICIAL: &str = "mcp_official";

/// The default page size when a caller does not ask for one.
const DEFAULT_PAGE_SIZE: u32 = 20;

/// One upstream catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegistrySource {
    /// The official `modelcontextprotocol/registry`.
    McpOfficial,
    /// Smithery.
    Smithery,
}

impl RegistrySource {
    /// The stable identifier stamped on this source's rows.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::McpOfficial => SOURCE_MCP_OFFICIAL,
            Self::Smithery => SOURCE_SMITHERY,
        }
    }

    /// Reads an identifier back, or `None` when it names no source here.
    #[must_use]
    pub fn parse(source: &str) -> Option<Self> {
        match source {
            SOURCE_MCP_OFFICIAL => Some(Self::McpOfficial),
            SOURCE_SMITHERY => Some(Self::Smithery),
            _ => None,
        }
    }
}

/// The upstream catalogs, and the state that makes paging them cheap.
///
/// Holds the cursor cache the official registry needs. That cache is a field
/// rather than a process global for the same reason the connection map is: two
/// hosts in one process would otherwise share it, and a test would inherit
/// whatever a previous test left in it.
#[derive(Debug)]
pub struct Registries {
    auth: McpRegistryAuthConfig,
    official: McpOfficialRegistry,
    smithery: SmitheryRegistry,
    /// Which cursor produced which page, keyed by query, page size, and page.
    cursors: Mutex<HashMap<(String, u32, u32), String>>,
}

impl Registries {
    /// Builds the dispatcher.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when an HTTP client cannot be built.
    pub fn new(auth: McpRegistryAuthConfig) -> Result<Self> {
        Ok(Self {
            official: McpOfficialRegistry::new()?,
            smithery: SmitheryRegistry::new()?,
            auth,
            cursors: Mutex::new(HashMap::new()),
        })
    }

    /// The sources that take part in a search.
    ///
    /// The official registry is always in and always first, so its rows lead a
    /// merged result. Smithery joins only when a key is configured — see the
    /// module note.
    #[must_use]
    pub fn searchable(&self) -> Vec<RegistrySource> {
        let mut sources = vec![RegistrySource::McpOfficial];
        if self.smithery_key().is_some() {
            sources.push(RegistrySource::Smithery);
        }
        sources
    }

    /// Searches one source.
    ///
    /// Returns the rows and a best-effort upper bound on the page count. A
    /// source that cannot know the true total reports the current page plus one
    /// while more results exist, which is enough for a caller to offer a "next"
    /// control without committing to a number it would have to walk the whole
    /// catalog to learn.
    ///
    /// # Errors
    ///
    /// Returns whatever the upstream returns.
    pub async fn search(
        &self,
        store: &Store,
        source: RegistrySource,
        query: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<RegistryServerSummary>, u32)> {
        let query = query.unwrap_or_default().trim();
        let page = page.max(1);
        let page_size = if page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            page_size
        };

        match source {
            RegistrySource::McpOfficial => {
                self.official
                    .search(store, &self.auth, &self.cursors, query, page, page_size)
                    .await
            }
            RegistrySource::Smithery => {
                self.smithery
                    .search(store, self.smithery_key().as_deref(), query, page, page_size)
                    .await
            }
        }
    }

    /// Fetches one server's detail from the source that lists it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when `source` names nothing here, plus
    /// whatever the upstream returns.
    pub async fn get(
        &self,
        store: &Store,
        source: &str,
        qualified_name: &str,
    ) -> Result<RegistryServerDetail> {
        let source = RegistrySource::parse(source).ok_or_else(|| Error::UnknownServer {
            server: format!("registry source `{source}`"),
        })?;

        match source {
            RegistrySource::McpOfficial => {
                self.official.get(store, &self.auth, qualified_name).await
            }
            // Deliberately not gated on the key: an already-installed Smithery
            // server must stay inspectable after the key is removed.
            RegistrySource::Smithery => {
                self.smithery
                    .get(store, self.smithery_key().as_deref(), qualified_name)
                    .await
            }
        }
    }

    /// The effective Smithery key: configuration first, then the environment.
    ///
    /// A blank value counts as unset, so an empty setting does not produce a
    /// bare `Bearer ` header that every request then fails on.
    #[must_use]
    pub fn smithery_key(&self) -> Option<String> {
        self.auth
            .smithery_api_key
            .clone()
            .filter(|key| !key.trim().is_empty())
            .or_else(|| non_blank_env("SMITHERY_API_KEY"))
    }
}

/// An environment variable, or `None` when it is unset or blank.
///
/// The environment fallback exists so container deployments that set only
/// variables keep working; breaking them to satisfy a principle would be a poor
/// trade.
pub(super) fn non_blank_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}
