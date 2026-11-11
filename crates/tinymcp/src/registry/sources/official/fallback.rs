//! What the official adapter serves when the registry cannot answer.
//!
//! In order: an earlier answer to the same request, however old; for the
//! first page of a search, rows from cached catalog pages and cached server
//! details that match the query; otherwise nothing, and the caller returns the
//! error.
//!
//! A listing that timed out also starts a cooldown for listings of its kind,
//! so the next keystrokes go straight to the cache instead of each waiting out
//! the same budget.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::types::{OfficialListResponse, OfficialServer};
use super::{BROWSE_CACHE_PREFIX, CursorCache, DETAIL_CACHE_PREFIX, search_cache_key, served};
use crate::error::Error;
use crate::registry::Store;
use crate::registry::curation::{CuratedCatalog, entry_summary};
use crate::registry::sources::types::{RegistryOperation, SourcePage};
use tinymcp_bus::{RegistryFreshness, RegistryServerSummary};

/// One timed-out listing, remembered until its cooldown ends.
#[derive(Debug, Clone)]
struct Stall {
    since: Instant,
    url: String,
    timeout: Duration,
}

/// The listings currently skipping the network, by kind.
#[derive(Debug, Default)]
pub(super) struct Cooldown {
    stalls: Mutex<HashMap<RegistryOperation, Stall>>,
}

impl Cooldown {
    /// Records that a listing against `url` ran out of `timeout`.
    pub(super) fn start(&self, operation: RegistryOperation, url: &str, timeout: Duration) {
        tracing::debug!(%operation, "official registry cooling down after a timeout");
        self.stalls.lock().insert(
            operation,
            Stall {
                since: Instant::now(),
                url: url.to_string(),
                timeout,
            },
        );
    }

    /// The timeout to report for `operation` against `url`, when it is still
    /// within `period` of the last one.
    pub(super) fn active(
        &self,
        operation: RegistryOperation,
        url: &str,
        period: Duration,
    ) -> Option<Error> {
        let mut stalls = self.stalls.lock();
        let stall = stalls.get(&operation)?;

        if stall.url != url || stall.since.elapsed() >= period {
            stalls.remove(&operation);
            return None;
        }

        Some(Error::RegistryTimeout {
            endpoint: crate::redact_endpoint(url),
            operation,
            timeout: stall.timeout,
        })
    }
}

/// What can stand in for a listing the registry could not answer.
pub(super) fn serve_cached(
    store: &Store,
    catalog: &CuratedCatalog,
    cursors: &CursorCache,
    query: &str,
    page: u32,
    page_size: u32,
) -> Option<SourcePage> {
    if let Some(page) = stale_page(store, cursors, query, page, page_size) {
        return Some(page);
    }
    if query.is_empty() || page != 1 {
        return None;
    }

    let servers = local_matches(store, catalog, query, page_size);
    tracing::debug!(
        query_length = query.len(),
        matches = servers.len(),
        "official search served from cached catalog pages and details"
    );

    (!servers.is_empty()).then_some(SourcePage {
        servers,
        total_pages: 1,
        freshness: RegistryFreshness::LocalFallback,
    })
}

/// An earlier answer to exactly this listing.
fn stale_page(
    store: &Store,
    cursors: &CursorCache,
    query: &str,
    page: u32,
    page_size: u32,
) -> Option<SourcePage> {
    let body = store
        .cached_stale(&search_cache_key(query, page, page_size))
        .ok()??;
    let parsed: OfficialListResponse = serde_json::from_str(&body).ok()?;

    tracing::debug!(
        page,
        page_size,
        "official listing served from a stale cache entry"
    );
    Some(SourcePage {
        freshness: RegistryFreshness::Cached,
        ..served(parsed, cursors, query, page, page_size)
    })
}

/// Rows from every cached catalog page and server detail that match `query`,
/// at most `limit`.
///
/// Catalog page rows come before detail rows, and a server appears once. A row
/// matches when every word of the query appears in its name, title, or
/// description, ignoring case. Rows matching on name or title come first.
fn local_matches(
    store: &Store,
    catalog: &CuratedCatalog,
    query: &str,
    limit: u32,
) -> Vec<RegistryServerSummary> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return Vec::new();
    }

    let pages = store
        .cached_with_prefix(BROWSE_CACHE_PREFIX)
        .unwrap_or_default();
    let details = store
        .cached_with_prefix(DETAIL_CACHE_PREFIX)
        .unwrap_or_default();
    let page_rows = pages
        .iter()
        .filter_map(|body| serde_json::from_str::<OfficialListResponse>(body).ok())
        .flat_map(OfficialListResponse::into_summaries);
    let detail_rows = details
        .iter()
        .filter_map(|body| serde_json::from_str::<OfficialServer>(body).ok())
        .filter(OfficialServer::is_installable)
        .map(OfficialServer::into_summary);

    let curated_rows = catalog.entries().iter().map(entry_summary);

    let mut seen = HashSet::new();
    let mut matches: Vec<(bool, RegistryServerSummary)> = page_rows
        .chain(detail_rows)
        .chain(curated_rows)
        .filter(|row| seen.insert(row.qualified_name.clone()))
        .filter_map(|row| {
            let label = format!("{} {}", row.qualified_name, row.display_name).to_lowercase();
            let description = row
                .description
                .as_deref()
                .unwrap_or_default()
                .to_lowercase();
            let in_label = terms.iter().all(|term| label.contains(term.as_str()));
            let anywhere = terms
                .iter()
                .all(|term| label.contains(term.as_str()) || description.contains(term.as_str()));
            anywhere.then_some((in_label, row))
        })
        .collect();

    matches.sort_by_key(|(in_label, row)| (!catalog.is_promoted(&row.qualified_name), !in_label));
    matches
        .into_iter()
        .map(|(_, row)| row)
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .collect()
}
