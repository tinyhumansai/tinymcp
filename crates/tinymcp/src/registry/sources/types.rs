//! The dispatcher over the upstream catalogs.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use super::official::McpOfficialRegistry;
use super::smithery::SmitheryRegistry;
use crate::error::{Error, Result};
use crate::registry::Store;
use tinymcp_bus::{
    McpRegistryAuthConfig, RegistryFreshness, RegistryServerDetail, RegistryServerSummary,
    RegistrySettings,
};

/// The identifier Smithery stamps on its rows.
pub const SOURCE_SMITHERY: &str = "smithery";

/// The identifier the official registry stamps on its rows.
pub const SOURCE_MCP_OFFICIAL: &str = "mcp_official";

/// The default page size when a caller does not ask for one.
const DEFAULT_PAGE_SIZE: u32 = 20;

/// What a request to an upstream catalog was asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RegistryOperation {
    /// A page of the unfiltered catalog.
    Browse,
    /// A page of results for a query.
    Search,
    /// One server's detail.
    Detail,
}

impl RegistryOperation {
    /// The operation's lowercase name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Browse => "browse",
            Self::Search => "search",
            Self::Detail => "detail",
        }
    }

    /// The operation a listing is: a search when there is a query, a browse
    /// otherwise.
    #[must_use]
    pub const fn for_query(query: &str) -> Self {
        if query.is_empty() {
            Self::Browse
        } else {
            Self::Search
        }
    }
}

impl fmt::Display for RegistryOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// How long the official catalog adapter waits on each kind of request.
///
/// Search has the shortest budget: it is what a user is typing into, and the
/// registry's search can stall while its plain listing answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryTimeouts {
    /// Establishing a connection.
    pub connect: Duration,
    /// A page of the unfiltered catalog.
    pub browse: Duration,
    /// A page of results for a query.
    pub search: Duration,
    /// One server's detail.
    pub detail: Duration,
    /// How long listings of the same kind skip the network after one timed
    /// out, answering from what is cached instead.
    pub cooldown: Duration,
}

impl RegistryTimeouts {
    /// The budget for `operation`.
    #[must_use]
    pub const fn budget(&self, operation: RegistryOperation) -> Duration {
        match operation {
            RegistryOperation::Browse => self.browse,
            RegistryOperation::Search => self.search,
            RegistryOperation::Detail => self.detail,
        }
    }
}

impl Default for RegistryTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            browse: Duration::from_secs(15),
            search: Duration::from_secs(8),
            detail: Duration::from_secs(12),
            cooldown: Duration::from_secs(60),
        }
    }
}

/// How the official catalog adapter keeps its local index of the catalog.
///
/// The index is what a search answers from once it exists, because the
/// registry's own search is far slower than paging its plain listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryIndexSettings {
    /// How old the last finished sync may be before a search or browse starts
    /// another in the background.
    pub refresh: Duration,
    /// The most pages one background run reads before it pauses. The next
    /// search or browse resumes the sync from where it paused.
    pub max_pages: u32,
    /// How many servers each page asks for.
    pub page_size: u32,
}

impl Default for RegistryIndexSettings {
    fn default() -> Self {
        Self {
            refresh: Duration::from_secs(6 * 60 * 60),
            max_pages: 200,
            page_size: 100,
        }
    }
}

/// One page from one upstream catalog.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourcePage {
    /// The rows on this page.
    pub servers: Vec<RegistryServerSummary>,
    /// A best-effort upper bound on the page count. See [`Registries::search`].
    pub total_pages: u32,
    /// Where the rows came from.
    pub freshness: RegistryFreshness,
}

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
    auth: Mutex<McpRegistryAuthConfig>,
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
        Self::with_official(auth, McpOfficialRegistry::new()?)
    }

    /// Builds the dispatcher over an official catalog adapter built with its
    /// own settings.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when the Smithery HTTP client cannot be
    /// built.
    pub fn with_official(
        mut auth: McpRegistryAuthConfig,
        official: McpOfficialRegistry,
    ) -> Result<Self> {
        auth.curated_servers =
            crate::registry::curation::validated_runtime_entries(auth.curated_servers);
        Ok(Self {
            official,
            smithery: SmitheryRegistry::new()?,
            auth: Mutex::new(auth),
            cursors: Mutex::new(HashMap::new()),
        })
    }

    /// The curated list in force: the built-in servers plus the host's
    /// validated runtime entries.
    #[must_use]
    pub fn curated_catalog(&self) -> crate::registry::curation::CuratedCatalog {
        crate::registry::curation::CuratedCatalog::for_settings(&self.auth.lock())
    }

    /// Starts a background sync of the official catalog's local index when
    /// one is due, returning whether it did.
    ///
    /// A search answers from the index once a sync has finished; until then it
    /// asks the registry. The sync never blocks the caller, and at most one
    /// runs at a time.
    pub fn refresh_index(&self, store: &Arc<Store>) -> bool {
        let auth = self.auth.lock().clone();
        self.official.refresh_index(store, &auth)
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
    /// Returns the rows, a best-effort upper bound on the page count, and
    /// where the rows came from. A source that cannot know the true total
    /// reports the current page plus one while more results exist, which is
    /// enough for a caller to offer a "next" control without committing to a
    /// number it would have to walk the whole catalog to learn.
    ///
    /// When the official registry cannot answer, the page is served from what
    /// is cached and its freshness says so; see
    /// [`RegistryFreshness`](tinymcp_bus::RegistryFreshness).
    ///
    /// # Errors
    ///
    /// Returns whatever the upstream returns when nothing cached can stand in.
    pub async fn search(
        &self,
        store: &Store,
        source: RegistrySource,
        query: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> Result<SourcePage> {
        let query = query.unwrap_or_default().trim();
        let page = page.max(1);
        let page_size = if page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            page_size
        };

        // Both values are taken out from under the lock before any await: a
        // guard held across one is not `Send`, and the whole service future
        // has to be.
        match source {
            RegistrySource::McpOfficial => {
                let auth = self.auth.lock().clone();
                self.official
                    .search(store, &auth, &self.cursors, query, page, page_size)
                    .await
            }
            RegistrySource::Smithery => {
                let key = self.smithery_key();
                let (servers, total_pages) = self
                    .smithery
                    .search(store, key.as_deref(), query, page, page_size)
                    .await?;
                Ok(SourcePage {
                    servers,
                    total_pages,
                    freshness: RegistryFreshness::Live,
                })
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
                let auth = self.auth.lock().clone();
                self.official.get(store, &auth, qualified_name).await
            }
            // Deliberately not gated on the key: an already-installed Smithery
            // server must stay inspectable after the key is removed.
            RegistrySource::Smithery => {
                let key = self.smithery_key();
                self.smithery
                    .get(store, key.as_deref(), qualified_name)
                    .await
            }
        }
    }

    /// Which registry credentials are configured, with no values.
    ///
    /// The credential fields are booleans. A getter that echoed a stored secret
    /// back would put it in whatever a caller does with a settings response —
    /// a form, a log, a diagnostic bundle.
    #[must_use]
    pub fn settings(&self) -> RegistrySettings {
        RegistrySettings {
            smithery_api_key_set: self.smithery_key().is_some(),
            mcp_official_token_set: self.official_token().is_some(),
            // Not a secret, and a user who cannot see which registry they are
            // pointed at cannot debug it.
            mcp_official_base: self
                .auth
                .lock()
                .mcp_official_base
                .clone()
                .filter(|base| !base.trim().is_empty()),
        }
    }

    /// The effective official-registry token: configuration first, then the
    /// environment.
    #[must_use]
    pub fn official_token(&self) -> Option<String> {
        self.auth
            .lock()
            .mcp_official_token
            .clone()
            .filter(|token| !token.trim().is_empty())
            .or_else(|| non_blank_env("MCP_OFFICIAL_REGISTRY_TOKEN"))
    }

    /// Replaces the registry credentials this dispatcher uses.
    ///
    /// Per field: `None` leaves the stored value alone, and `Some` sets it —
    /// where a blank string clears it, falling back to the environment.
    ///
    /// This changes only what *this* process uses. Persisting the settings is
    /// the host's: it owns where its configuration lives, and a module writing
    /// into that would be reaching into a file it does not own.
    pub fn set_settings(
        &self,
        smithery_api_key: Option<String>,
        mcp_official_base: Option<String>,
        mcp_official_token: Option<String>,
    ) {
        /// A blank update clears the field; an absent one leaves it.
        fn apply(field: &mut Option<String>, update: Option<String>) {
            if let Some(value) = update {
                let trimmed = value.trim();
                *field = (!trimmed.is_empty()).then(|| trimmed.to_string());
            }
        }

        let mut auth = self.auth.lock();
        apply(&mut auth.smithery_api_key, smithery_api_key);
        apply(&mut auth.mcp_official_base, mcp_official_base);
        apply(&mut auth.mcp_official_token, mcp_official_token);
    }

    /// The effective Smithery key: configuration first, then the environment.
    ///
    /// A blank value counts as unset, so an empty setting does not produce a
    /// bare `Bearer ` header that every request then fails on.
    #[must_use]
    pub fn smithery_key(&self) -> Option<String> {
        self.auth
            .lock()
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
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}
