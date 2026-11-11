//! The official `modelcontextprotocol/registry` catalog.
//!
//! `GET /v0/servers?version=latest` lists one row per server;
//! `GET /v0/servers/{name}/versions` details — the registry has no
//! single-server endpoint, so a detail lookup reads the version list and takes
//! the one marked latest.
//!
//! # Pages over cursors
//!
//! The registry pages by opaque cursor: each response carries the token for the
//! next page, or none when the results end. Callers here ask for numbered
//! pages, so the adapter keeps a map from page to the cursor that produced it.
//!
//! Asking for page N with a warm map costs one request. With a cold map — after
//! a restart, or a link straight to page N — the adapter walks forward from
//! page one, filling the map as it goes. The walk stops at
//! [`MAX_CURSOR_WALK_PAGES`] rather than fan one request into hundreds; a
//! caller that needs to go deeper should page sequentially, which builds the
//! map naturally.
//!
//! The walk also consults the stored response cache before making a request,
//! so a cold in-memory map after a restart does not mean a cold network.
//!
//! # When the registry cannot answer
//!
//! Each kind of request has its own time budget ([`RegistryTimeouts`]). A
//! listing that times out, cannot connect, or is answered 408, 429 or 5xx is
//! served from the cache instead, and its freshness says which kind of answer
//! it is; a timed-out listing also skips the network for a cooldown. The
//! `fallback` module holds the order.
//!
//! # Searching a local index
//!
//! The registry's search is far slower than its plain listing, so the adapter
//! keeps a local copy of the listing and answers queries from it once a sync
//! has finished. The `index` module holds how it syncs and ranks.
//!
//! # The page count is a bound, not a total
//!
//! Knowing the true total would mean walking the whole cursor chain, which is
//! the cost this design exists to avoid. The adapter reports one page beyond
//! the current one while more results exist, which is what a caller needs to
//! decide whether to offer a "next" control.

mod fallback;
mod index;
mod types;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use self::fallback::{Cooldown, serve_cached};
use self::index::OfficialIndex;
use self::types::{OfficialListResponse, OfficialServer, latest_version};
use super::encode::encode_path_segment;
use super::shared::{cache, read_body};
use super::types::{
    RegistryIndexSettings, RegistryOperation, RegistryTimeouts, SourcePage, non_blank_env,
};
use crate::error::{Error, Result};
use crate::registry::Store;
use crate::registry::curation::{CuratedCatalog, entry_detail, entry_summary};
use tinymcp_bus::{
    McpRegistryAuthConfig, RegistryFreshness, RegistryServerDetail, RegistryServerSummary,
};

/// Where the registry lives when nothing overrides it.
const DEFAULT_BASE: &str = "https://registry.modelcontextprotocol.io";

/// How far the adapter will walk to reach a deep page with a cold map.
///
/// At fifty rows a page this reaches the two-thousand-five-hundredth result.
/// Past that a single request would fan into hundreds upstream, which is a
/// denial of service aimed at someone else.
const MAX_CURSOR_WALK_PAGES: u32 = 50;

/// The cache key prefix every page of the unfiltered catalog shares.
const BROWSE_CACHE_PREFIX: &str = "mcp_official:search:latest::";

/// The cache key prefix every server detail shares.
const DETAIL_CACHE_PREFIX: &str = "mcp_official:detail:";

/// The map from page to the cursor that produced it.
type CursorCache = Mutex<HashMap<(String, u32, u32), String>>;

/// The official catalog adapter.
#[derive(Debug)]
pub struct McpOfficialRegistry {
    http: reqwest::Client,
    timeouts: RegistryTimeouts,
    cooldown: Cooldown,
    index: Arc<OfficialIndex>,
}

impl McpOfficialRegistry {
    /// Builds the adapter with the default [`RegistryTimeouts`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when the HTTP client cannot be built.
    pub fn new() -> Result<Self> {
        Self::with_timeouts(RegistryTimeouts::default())
    }

    /// Builds the adapter with its own time budgets.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when the HTTP client cannot be built.
    pub fn with_timeouts(timeouts: RegistryTimeouts) -> Result<Self> {
        Self::with_settings(timeouts, RegistryIndexSettings::default())
    }

    /// Builds the adapter with its own time budgets and index settings.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when the HTTP client cannot be built.
    pub fn with_settings(timeouts: RegistryTimeouts, index: RegistryIndexSettings) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .build()
            .map_err(|source| Error::ClientBuild {
                source: Box::new(source.without_url()),
            })?;

        Ok(Self {
            index: Arc::new(OfficialIndex::new(http.clone(), timeouts, index)),
            http,
            timeouts,
            cooldown: Cooldown::default(),
        })
    }

    /// Starts a background sync of the local catalog index when one is due,
    /// returning whether it did. See the `index` module.
    pub(super) fn refresh_index(&self, store: &Arc<Store>, auth: &McpRegistryAuthConfig) -> bool {
        self.index.refresh(store, auth)
    }

    /// Searches the catalog.
    ///
    /// Once the local index has finished a sync of the configured catalog, a
    /// query is answered from it without asking the registry.
    ///
    /// When the registry cannot answer — it timed out, was unreachable, or
    /// answered 408, 429 or 5xx — the page is served from the cache instead,
    /// with its freshness saying so. The fallback module sets out the order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when a deep page is asked for with
    /// a cold map, and whatever the upstream returns when nothing cached can
    /// stand in.
    pub(super) async fn search(
        &self,
        store: &Store,
        auth: &McpRegistryAuthConfig,
        cursors: &CursorCache,
        query: &str,
        page: u32,
        page_size: u32,
    ) -> Result<SourcePage> {
        let catalog = CuratedCatalog::for_settings(auth);
        if let Some(found) = index::search(store, &base_url(auth), &catalog, query, page, page_size)
        {
            return Ok(found);
        }

        let mut found = self
            .search_registry(store, auth, cursors, query, page, page_size)
            .await?;
        if page == 1 {
            // A local fallback is one bounded page with nothing after it, so it
            // keeps its size; a registry page is followed by its cursor.
            let cap = (found.freshness == RegistryFreshness::LocalFallback)
                .then(|| usize::try_from(page_size.max(1)).unwrap_or(usize::MAX));
            lead_with_curated(&mut found.servers, &catalog, query, cap);
        }
        Ok(found)
    }

    /// Searches without the index: the cache, then the registry, then the
    /// fallback.
    async fn search_registry(
        &self,
        store: &Store,
        auth: &McpRegistryAuthConfig,
        cursors: &CursorCache,
        query: &str,
        page: u32,
        page_size: u32,
    ) -> Result<SourcePage> {
        let cache_key = search_cache_key(query, page, page_size);
        let catalog = CuratedCatalog::for_settings(auth);

        if let Ok(Some(cached)) = store.cached(&cache_key)
            && let Ok(parsed) = serde_json::from_str::<OfficialListResponse>(&cached)
        {
            tracing::debug!(page, page_size, "official search cache hit");
            return Ok(served(parsed, cursors, query, page, page_size));
        }

        let operation = RegistryOperation::for_query(query);
        let url = list_url(auth);

        if let Some(error) = self
            .cooldown
            .active(operation, &url, self.timeouts.cooldown)
        {
            tracing::debug!(%operation, "official registry cooling down; answering from the cache");
            return serve_cached(store, &catalog, cursors, query, page, page_size).ok_or(error);
        }

        match self
            .search_live(store, auth, cursors, query, page, page_size)
            .await
        {
            Err(error) if error.is_registry_unavailable() => {
                if error.is_timeout() {
                    self.cooldown
                        .start(operation, &url, self.timeouts.budget(operation));
                }
                tracing::debug!(
                    %operation,
                    code = error.wire_name(),
                    "official registry unavailable; answering from the cache"
                );
                serve_cached(store, &catalog, cursors, query, page, page_size).ok_or(error)
            }
            outcome => outcome,
        }
    }

    /// Searches the registry itself, filling the caches on the way.
    async fn search_live(
        &self,
        store: &Store,
        auth: &McpRegistryAuthConfig,
        cursors: &CursorCache,
        query: &str,
        page: u32,
        page_size: u32,
    ) -> Result<SourcePage> {
        let cache_key = search_cache_key(query, page, page_size);

        let cursor = match page {
            1 => None,
            _ => match recall_cursor(cursors, query, page_size, page - 1) {
                Some(cursor) => Some(cursor),
                None => {
                    match self
                        .walk_to(store, auth, cursors, query, page_size, page)
                        .await?
                    {
                        Some(cursor) => Some(cursor),
                        // The chain ended before reaching the page asked for.
                        // An empty result reporting this page as the last is
                        // what stops a caller paging further.
                        None => {
                            return Ok(SourcePage {
                                servers: Vec::new(),
                                total_pages: page,
                                freshness: RegistryFreshness::Live,
                            });
                        }
                    }
                }
            },
        };

        let body = self
            .fetch_page(auth, query, page_size, cursor.as_deref())
            .await?;
        let parsed: OfficialListResponse = serde_json::from_str(&body)
            .map_err(|error| Error::malformed(format!("official list response: {error}")))?;
        cache(store, &cache_key, &body);

        Ok(served(parsed, cursors, query, page, page_size))
    }

    /// Fetches one server's detail.
    ///
    /// A curated server the registry cannot describe — it does not list it, or
    /// cannot be reached — is described from its curated entry instead.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the registry does not list it (a
    /// 404, or no version), plus whatever the upstream returns.
    pub(super) async fn get(
        &self,
        store: &Store,
        auth: &McpRegistryAuthConfig,
        qualified_name: &str,
    ) -> Result<RegistryServerDetail> {
        match self.get_listed(store, auth, qualified_name).await {
            Err(error) => match CuratedCatalog::for_settings(auth).get(qualified_name) {
                Some(curated) => {
                    tracing::debug!(
                        qualified_name,
                        code = error.wire_name(),
                        "official detail answered from the curated entry"
                    );
                    Ok(entry_detail(curated))
                }
                None => Err(error),
            },
            found => found,
        }
    }

    /// Fetches one server's detail from the registry or its cache.
    async fn get_listed(
        &self,
        store: &Store,
        auth: &McpRegistryAuthConfig,
        qualified_name: &str,
    ) -> Result<RegistryServerDetail> {
        let cache_key = format!("{DETAIL_CACHE_PREFIX}{qualified_name}");

        if let Ok(Some(cached)) = store.cached(&cache_key)
            && let Ok(server) = serde_json::from_str::<OfficialServer>(&cached)
        {
            tracing::debug!(qualified_name, "official detail cache hit");
            return Ok(server.into_detail());
        }

        let url = format!(
            "{}/v0/servers/{}/versions",
            base_url(auth),
            encode_path_segment(qualified_name)
        );
        // The registry answers 404 for a name it does not list. That is the
        // same answer as an empty versions document, not an upstream failure.
        let body = match self
            .send(self.request(auth, &url), &url, RegistryOperation::Detail)
            .await
        {
            Err(Error::Http { status: 404, .. }) => {
                return Err(Error::UnknownServer {
                    server: qualified_name.to_string(),
                });
            }
            sent => sent?,
        };

        let document: Value = serde_json::from_str(&body)
            .map_err(|error| Error::malformed(format!("official versions response: {error}")))?;

        let latest = latest_version(&document).ok_or_else(|| Error::UnknownServer {
            server: qualified_name.to_string(),
        })?;

        // Cached as the inner object, which is what the hit path above reads.
        cache(store, &cache_key, &latest.to_string());

        let server: OfficialServer = serde_json::from_value(latest.clone())
            .map_err(|error| Error::malformed(format!("official server record: {error}")))?;

        Ok(server.into_detail())
    }

    /// Walks forward from page one until the cursor for `target` is known.
    ///
    /// Returns the cursor to send for `target`, or `None` when the chain ran
    /// out first. Fills the map as it goes, so the pages after this one cost
    /// one request each.
    async fn walk_to(
        &self,
        store: &Store,
        auth: &McpRegistryAuthConfig,
        cursors: &CursorCache,
        query: &str,
        page_size: u32,
        target: u32,
    ) -> Result<Option<String>> {
        if target <= 1 {
            return Ok(None);
        }
        if target > MAX_CURSOR_WALK_PAGES {
            return Err(Error::malformed(format!(
                "page {target} is beyond the {MAX_CURSOR_WALK_PAGES} this registry will walk to; \
                 page sequentially to reach it"
            )));
        }

        tracing::debug!(target, page_size, "walking the official registry cursors");

        let mut cursor: Option<String> = None;
        for page in 1..target {
            let cache_key = search_cache_key(query, page, page_size);

            // The stored cache first: after a restart the in-memory map is
            // empty but a previous run's page bodies may still be on disk, and
            // using them removes network calls that have nothing to do with
            // what the network currently holds.
            let body = if let Ok(Some(body)) = store.cached(&cache_key) {
                body
            } else {
                let body = self
                    .fetch_page(auth, query, page_size, cursor.as_deref())
                    .await?;
                cache(store, &cache_key, &body);
                body
            };

            let parsed: OfficialListResponse = serde_json::from_str(&body)
                .map_err(|error| Error::malformed(format!("official list response: {error}")))?;

            match parsed.next_cursor() {
                Some(next) => {
                    remember_cursor(cursors, query, page_size, page, next.to_string());
                    cursor = Some(next.to_string());
                }
                None => return Ok(None),
            }
        }

        Ok(cursor)
    }

    /// Fetches one page.
    async fn fetch_page(
        &self,
        auth: &McpRegistryAuthConfig,
        query: &str,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<String> {
        // The query is what a user typed. Its presence and length are logged;
        // its text is not, so a search does not end up in a log aggregator.
        tracing::debug!(
            has_query = !query.is_empty(),
            query_length = query.len(),
            limit,
            has_cursor = cursor.is_some(),
            "fetching an official registry page"
        );

        let url = list_url(auth);
        let mut request = self
            .request(auth, &url)
            .query(&[("limit", limit.to_string())])
            .query(&[("version", "latest")]);
        // The registry refuses a search shorter than its minimum with a 422,
        // so a query that short is served as the unfiltered page instead.
        if query.trim().chars().count() >= MIN_SEARCH_CHARS {
            request = request.query(&[("search", query)]);
        }
        if let Some(cursor) = cursor {
            request = request.query(&[("cursor", cursor)]);
        }

        self.send(request, &url, RegistryOperation::for_query(query))
            .await
    }

    /// A request carrying the accept header and any configured token.
    fn request(&self, auth: &McpRegistryAuthConfig, url: &str) -> reqwest::RequestBuilder {
        let request = self.http.get(url).header("Accept", "application/json");
        match auth_token(auth) {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    /// Sends a request within `operation`'s budget and returns its body.
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        url: &str,
        operation: RegistryOperation,
    ) -> Result<String> {
        let timeout = self.timeouts.budget(operation);
        read_body(request.timeout(timeout), url, operation, timeout).await
    }
}

/// A parsed page as served, recording the cursor it carries.
fn served(
    parsed: OfficialListResponse,
    cursors: &CursorCache,
    query: &str,
    page: u32,
    page_size: u32,
) -> SourcePage {
    let has_next = parsed.next_cursor().is_some();
    if let Some(cursor) = parsed.next_cursor() {
        remember_cursor(cursors, query, page_size, page, cursor.to_string());
    }

    SourcePage {
        servers: parsed.into_summaries(),
        total_pages: page_bound(page, has_next),
        freshness: RegistryFreshness::Live,
    }
}

/// Puts the curated servers matching `query` at the head of a first page,
/// adding any the page does not carry and keeping the registry's row for any
/// it does.
///
/// A registry page is not cut to fit: page two continues from the registry's
/// own cursor, so a row pushed off page one would never appear on any page,
/// and page one may run longer by the curated servers it adds. Only a page
/// with nothing after it (the local fallback) is cut back to `cap`.
///
/// Only promoted entries lead (see [`CuratedCatalog::is_promoted`]); a runtime
/// entry is not moved to the top.
fn lead_with_curated(
    servers: &mut Vec<RegistryServerSummary>,
    catalog: &CuratedCatalog,
    query: &str,
    cap: Option<usize>,
) {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return;
    }

    let mut leading: Vec<RegistryServerSummary> = catalog
        .promoted()
        .filter(|curated| {
            let text = format!(
                "{} {} {}",
                curated.qualified_name, curated.display_name, curated.description
            )
            .to_lowercase();
            terms.iter().all(|term| text.contains(term.as_str()))
        })
        .map(|curated| {
            servers
                .iter()
                .position(|row| row.qualified_name == curated.qualified_name)
                .map_or_else(|| entry_summary(curated), |at| servers.remove(at))
        })
        .collect();
    leading.append(servers);
    if let Some(cap) = cap {
        leading.truncate(cap);
    }
    *servers = leading;
}

/// The list endpoint.
fn list_url(auth: &McpRegistryAuthConfig) -> String {
    format!("{}/v0/servers", base_url(auth))
}

/// The shortest `search` the official registry accepts, counted after
/// trimming. A shorter one is answered `422 search must contain at least 3
/// characters after trimming`.
const MIN_SEARCH_CHARS: usize = 3;

/// The cache key for one page of one search.
fn search_cache_key(query: &str, page: u32, page_size: u32) -> String {
    format!("mcp_official:search:latest:{query}:{page}:{page_size}")
}

/// Records which cursor produced a page.
fn remember_cursor(cursors: &CursorCache, query: &str, page_size: u32, page: u32, cursor: String) {
    cursors
        .lock()
        .insert((query.to_string(), page_size, page), cursor);
}

/// Recalls which cursor produced a page.
fn recall_cursor(cursors: &CursorCache, query: &str, page_size: u32, page: u32) -> Option<String> {
    cursors
        .lock()
        .get(&(query.to_string(), page_size, page))
        .cloned()
}

/// The best-effort page count. See the module note.
fn page_bound(page: u32, has_next: bool) -> u32 {
    if has_next {
        page.saturating_add(1)
    } else {
        page
    }
}

/// The effective registry base: configuration first, then the environment,
/// then the default.
fn base_url(auth: &McpRegistryAuthConfig) -> String {
    auth.mcp_official_base
        .clone()
        .filter(|base| !base.trim().is_empty())
        .or_else(|| non_blank_env("MCP_OFFICIAL_REGISTRY_BASE"))
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

/// The effective registry token: configuration first, then the environment.
fn auth_token(auth: &McpRegistryAuthConfig) -> Option<String> {
    auth.mcp_official_token
        .clone()
        .filter(|token| !token.trim().is_empty())
        .or_else(|| non_blank_env("MCP_OFFICIAL_REGISTRY_TOKEN"))
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
