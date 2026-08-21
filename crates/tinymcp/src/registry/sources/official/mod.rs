//! The official `modelcontextprotocol/registry` catalog.
//!
//! `GET /v0/servers` lists; `GET /v0/servers/{name}/versions` details — the
//! registry has no single-server endpoint, so a detail lookup reads the version
//! list and takes the newest.
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
//! # The page count is a bound, not a total
//!
//! Knowing the true total would mean walking the whole cursor chain, which is
//! the cost this design exists to avoid. The adapter reports one page beyond
//! the current one while more results exist, which is what a caller needs to
//! decide whether to offer a "next" control.

mod types;

use std::collections::HashMap;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;

use self::types::{OfficialListResponse, OfficialServer};
use super::encode::encode_path_segment;
use super::shared::{MAX_ERROR_BODY_BYTES, cache, truncate};
use super::types::non_blank_env;
use crate::error::{Error, Result};
use crate::registry::Store;
use tinymcp_bus::{McpRegistryAuthConfig, RegistryServerDetail, RegistryServerSummary};

/// Where the registry lives when nothing overrides it.
const DEFAULT_BASE: &str = "https://registry.modelcontextprotocol.io";

/// How long to wait on the registry.
const TIMEOUT: Duration = Duration::from_secs(15);

/// How far the adapter will walk to reach a deep page with a cold map.
///
/// At fifty rows a page this reaches the two-thousand-five-hundredth result.
/// Past that a single request would fan into hundreds upstream, which is a
/// denial of service aimed at someone else.
const MAX_CURSOR_WALK_PAGES: u32 = 50;

/// The map from page to the cursor that produced it.
type CursorCache = Mutex<HashMap<(String, u32, u32), String>>;

/// The official catalog adapter.
#[derive(Debug)]
pub struct McpOfficialRegistry {
    http: reqwest::Client,
}

impl McpOfficialRegistry {
    /// Builds the adapter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when the HTTP client cannot be built.
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|source| Error::ClientBuild {
                source: Box::new(source.without_url()),
            })?;

        Ok(Self { http })
    }

    /// Searches the catalog.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when a deep page is asked for with
    /// a cold map, plus whatever the upstream returns.
    pub(super) async fn search(
        &self,
        store: &Store,
        auth: &McpRegistryAuthConfig,
        cursors: &CursorCache,
        query: &str,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<RegistryServerSummary>, u32)> {
        let cache_key = search_cache_key(query, page, page_size);

        if let Ok(Some(cached)) = store.cached(&cache_key)
            && let Ok(parsed) = serde_json::from_str::<OfficialListResponse>(&cached)
        {
            tracing::debug!(page, page_size, "official search cache hit");
            let total_pages = page_bound(page, parsed.next_cursor().is_some());
            if let Some(cursor) = parsed.next_cursor() {
                remember_cursor(cursors, query, page_size, page, cursor.to_string());
            }
            return Ok((parsed.into_summaries(), total_pages));
        }

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
                        None => return Ok((Vec::new(), page)),
                    }
                }
            },
        };

        let body = self
            .fetch_page(auth, query, page_size, cursor.as_deref())
            .await?;
        let parsed: OfficialListResponse = serde_json::from_str(&body)
            .map_err(|error| Error::malformed(format!("official list response: {error}")))?;

        let next_cursor = parsed.next_cursor().map(ToString::to_string);
        if let Some(cursor) = next_cursor.clone() {
            remember_cursor(cursors, query, page_size, page, cursor);
        }
        cache(store, &cache_key, &body);

        Ok((
            parsed.into_summaries(),
            page_bound(page, next_cursor.is_some()),
        ))
    }

    /// Fetches one server's detail.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the registry lists no version of
    /// it, plus whatever the upstream returns.
    pub(super) async fn get(
        &self,
        store: &Store,
        auth: &McpRegistryAuthConfig,
        qualified_name: &str,
    ) -> Result<RegistryServerDetail> {
        let cache_key = format!("mcp_official:detail:{qualified_name}");

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
        let body = self.send(self.request(auth, &url), &url).await?;

        let document: Value = serde_json::from_str(&body)
            .map_err(|error| Error::malformed(format!("official versions response: {error}")))?;

        // The versions endpoint answers with the same envelope array as the
        // list endpoint; the newest version leads it.
        let newest = document
            .pointer("/servers/0/server")
            .ok_or_else(|| Error::UnknownServer {
                server: qualified_name.to_string(),
            })?;

        // Cached as the inner object, which is what the hit path above reads.
        cache(store, &cache_key, &newest.to_string());

        let server: OfficialServer = serde_json::from_value(newest.clone())
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
            let body = match store.cached(&cache_key) {
                Ok(Some(body)) => body,
                _ => {
                    let body = self
                        .fetch_page(auth, query, page_size, cursor.as_deref())
                        .await?;
                    cache(store, &cache_key, &body);
                    body
                }
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

        let url = format!("{}/v0/servers", base_url(auth));
        let mut request = self
            .request(auth, &url)
            .query(&[("limit", limit.to_string())]);
        if !query.is_empty() {
            request = request.query(&[("search", query)]);
        }
        if let Some(cursor) = cursor {
            request = request.query(&[("cursor", cursor)]);
        }

        self.send(request, &url).await
    }

    /// A request carrying the accept header and any configured token.
    fn request(&self, auth: &McpRegistryAuthConfig, url: &str) -> reqwest::RequestBuilder {
        let request = self.http.get(url).header("Accept", "application/json");
        match auth_token(auth) {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    /// Sends a request and returns its body, judging the status first.
    async fn send(&self, request: reqwest::RequestBuilder, url: &str) -> Result<String> {
        let response = request
            .send()
            .await
            .map_err(|error| Error::transport(url, error))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| Error::transport(url, error))?;

        if !status.is_success() {
            return Err(Error::Http {
                endpoint: crate::redact_endpoint(url),
                status: status.as_u16(),
                body: truncate(&body, MAX_ERROR_BODY_BYTES),
            });
        }

        Ok(body)
    }
}

/// The cache key for one page of one search.
fn search_cache_key(query: &str, page: u32, page_size: u32) -> String {
    format!("mcp_official:search:{query}:{page}:{page_size}")
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
mod test;
