//! The local index of the official catalog, and searching it.
//!
//! The registry's `search=` can take tens of seconds, while paging its plain
//! listing by cursor answers quickly. So the adapter pages the whole listing
//! into the store in the background and answers a search from that copy
//! instantly, reporting [`RegistryFreshness::Indexed`].
//!
//! # Syncing
//!
//! A sync walks `/v0/servers?version=latest` by cursor until the cursor runs
//! out, writing each page as it arrives. Each page has the browse budget. One
//! run reads at most [`RegistryIndexSettings::max_pages`] pages and then
//! pauses; the next search or browse resumes it from the stored cursor. A page
//! that fails stops the sync with what it wrote kept, and the next sync resumes
//! from the cursor it stopped at once the registry cooldown has passed. Only a
//! sync whose cursor ran out is finished.
//!
//! A search or a browse starts a sync when none has finished for the
//! configured catalog, when one was left unfinished, or when the last one is
//! older than [`RegistryIndexSettings::refresh`]. It runs on its own task so
//! the request that started it never waits for it, and at most one runs per
//! adapter.
//!
//! # Searching
//!
//! Until a sync has finished the index is not used, so a half-filled copy is
//! never presented as the whole catalog. After that, a row matches when every
//! word of the query appears in its name, title or description. Curated
//! servers match too even when the index lacks them. Matches are ranked
//! curated first, then name or title matches, then description matches, each
//! group alphabetical.

use std::cmp::Ordering as Order;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use serde_json::Value;

use super::types::{OfficialServer, latest_server_records};
use super::{auth_token, base_url, list_url};
use crate::error::{Error, Result};
use crate::registry::Store;
use crate::registry::curation::{CURATED_SERVERS, curated_server};
use crate::registry::sources::shared::read_body;
use crate::registry::sources::types::{
    RegistryIndexSettings, RegistryOperation, RegistryTimeouts, SOURCE_MCP_OFFICIAL, SourcePage,
};
use crate::registry::store::index::IndexRow;
use crate::registry::store::now_ms;
use tinymcp_bus::{McpRegistryAuthConfig, RegistryFreshness, RegistryServerSummary};

/// The source the index rows are stored under.
const INDEX_SOURCE: &str = SOURCE_MCP_OFFICIAL;

/// The official catalog's index: how it syncs, and whether a sync is running.
#[derive(Debug)]
pub(super) struct OfficialIndex {
    http: reqwest::Client,
    timeouts: RegistryTimeouts,
    settings: RegistryIndexSettings,
    in_flight: AtomicBool,
    retry_at: Mutex<Option<Instant>>,
}

/// Clears the in-flight flag when the sync holding it ends, however it ends.
struct InFlight(Arc<OfficialIndex>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.store(false, Ordering::Release);
    }
}

impl OfficialIndex {
    /// An index syncing over `http` within `timeouts`.
    pub(super) const fn new(
        http: reqwest::Client,
        timeouts: RegistryTimeouts,
        settings: RegistryIndexSettings,
    ) -> Self {
        Self {
            http,
            timeouts,
            settings,
            in_flight: AtomicBool::new(false),
            retry_at: Mutex::new(None),
        }
    }

    /// Starts a background sync when one is due, returning whether it did.
    ///
    /// Nothing starts while another sync of this index is running, within the
    /// cooldown after one failed, or outside a Tokio runtime.
    pub(super) fn refresh(
        self: &Arc<Self>,
        store: &Arc<Store>,
        auth: &McpRegistryAuthConfig,
    ) -> bool {
        if !self.is_due(store, &base_url(auth)) {
            return false;
        }
        if self
            .retry_at
            .lock()
            .is_some_and(|retry_at| Instant::now() < retry_at)
        {
            tracing::debug!("official catalog index sync is cooling down after a failure");
            return false;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }

        let guard = InFlight(Arc::clone(self));
        let store = Arc::clone(store);
        let auth = auth.clone();
        tracing::debug!("starting an official catalog index sync in the background");
        runtime.spawn(async move {
            let index = &guard.0;
            match index.sync(&store, &auth).await {
                Ok(pages) => {
                    *index.retry_at.lock() = None;
                    tracing::debug!(pages, "official catalog index synced");
                }
                Err(error) => {
                    *index.retry_at.lock() = Some(Instant::now() + index.timeouts.cooldown);
                    tracing::debug!(
                        code = error.wire_name(),
                        "official catalog index sync stopped; a later request resumes it: {error}"
                    );
                }
            }
            drop(guard);
        });
        true
    }

    /// Whether a sync should start for the catalog at `base`.
    fn is_due(&self, store: &Store, base: &str) -> bool {
        let state = match store.index_state(INDEX_SOURCE) {
            Ok(state) => state,
            Err(error) => {
                tracing::debug!("could not read the official catalog index state: {error}");
                return false;
            }
        };
        let Some(state) = state else {
            return true;
        };
        let refresh_ms = i64::try_from(self.settings.refresh.as_millis()).unwrap_or(i64::MAX);

        state.base_url != base
            || state.in_progress()
            || state
                .synced_ms
                .is_none_or(|synced_ms| now_ms().saturating_sub(synced_ms) >= refresh_ms)
    }

    /// Syncs the index, resuming an unfinished sync of the same catalog.
    ///
    /// Returns how many pages the sync holds so far, finished or paused at the
    /// page limit.
    ///
    /// # Errors
    ///
    /// Returns the upstream's error for a page that fails, after keeping every
    /// page before it; [`Error::MalformedResponse`] for a page that is not a
    /// list; and [`Error::Store`] when the index cannot be written.
    pub(super) async fn sync(&self, store: &Store, auth: &McpRegistryAuthConfig) -> Result<u32> {
        let base = base_url(auth);
        let resume = store
            .index_state(INDEX_SOURCE)?
            .filter(|state| state.in_progress() && state.base_url == base);

        let (mut cursor, mut pages) = if let Some(state) = resume {
            tracing::debug!(
                pages = state.pages,
                "resuming the official catalog index sync"
            );
            (state.cursor, state.pages)
        } else {
            store.begin_index_sync(INDEX_SOURCE, &base)?;
            (None, 0)
        };
        let mut finished = pages > 0 && cursor.is_none();
        let mut fetched: u32 = 0;

        while !finished {
            if fetched >= self.settings.max_pages {
                tracing::debug!(
                    pages,
                    "official catalog index sync paused at its page limit; a later request resumes it"
                );
                return Ok(pages);
            }

            let body = self.fetch(auth, cursor.as_deref()).await?;
            let document: Value = serde_json::from_str(&body)
                .map_err(|error| Error::malformed(format!("official list response: {error}")))?;
            if !document.get("servers").is_some_and(Value::is_array) {
                return Err(Error::malformed(
                    "official list response has no server list",
                ));
            }

            let next = document
                .pointer("/metadata/nextCursor")
                .and_then(Value::as_str)
                .filter(|next| !next.is_empty())
                .map(ToString::to_string);
            if next.is_some() && next == cursor {
                return Err(Error::malformed(
                    "official list response repeats the cursor it was asked for",
                ));
            }
            store.store_index_page(INDEX_SOURCE, &index_rows(&document), next.as_deref())?;

            pages += 1;
            fetched += 1;
            finished = next.is_none();
            cursor = next;
        }

        store.finish_index_sync(INDEX_SOURCE)?;
        Ok(pages)
    }

    /// Fetches one listing page for the sync, within the browse budget.
    async fn fetch(&self, auth: &McpRegistryAuthConfig, cursor: Option<&str>) -> Result<String> {
        let url = list_url(auth);
        let mut request = self
            .http
            .get(&url)
            .header("Accept", "application/json")
            .query(&[("limit", self.settings.page_size.to_string())])
            .query(&[("version", "latest")]);
        if let Some(token) = auth_token(auth) {
            request = request.bearer_auth(token);
        }
        if let Some(cursor) = cursor {
            request = request.query(&[("cursor", cursor)]);
        }

        let timeout = self.timeouts.browse;
        read_body(
            request.timeout(timeout),
            &url,
            RegistryOperation::Browse,
            timeout,
        )
        .await
    }
}

/// Whether a finished sync of the catalog at `base` is in the store.
pub(super) fn is_ready(store: &Store, base: &str) -> bool {
    matches!(
        store.index_state(INDEX_SOURCE),
        Ok(Some(state)) if state.base_url == base && state.synced_ms.is_some()
    )
}

/// One page of `query`'s matches from the index of the catalog at `base`, or
/// `None` when that index is not ready or the query has no words.
pub(super) fn search(
    store: &Store,
    base: &str,
    query: &str,
    page: u32,
    page_size: u32,
) -> Option<SourcePage> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() || !is_ready(store, base) {
        return None;
    }

    let hits = match store.search_index(INDEX_SOURCE, &terms) {
        Ok(hits) => hits,
        Err(error) => {
            tracing::debug!("could not search the official catalog index: {error}");
            return None;
        }
    };

    let mut matches: Vec<(bool, RegistryServerSummary)> = hits
        .into_iter()
        .filter_map(|hit| {
            let server: OfficialServer = serde_json::from_str(&hit.record_json).ok()?;
            Some((hit.in_label, server.into_summary()))
        })
        .collect();

    let indexed: HashSet<String> = matches
        .iter()
        .map(|(_, row)| row.qualified_name.clone())
        .collect();
    matches.extend(
        CURATED_SERVERS
            .iter()
            .filter(|curated| !indexed.contains(curated.qualified_name))
            .filter_map(|curated| {
                let row = curated.to_summary();
                let label = search_label(&row.qualified_name, &row.display_name);
                let description = curated.description.to_lowercase();
                let in_label = terms.iter().all(|term| label.contains(term.as_str()));
                let anywhere = terms.iter().all(|term| {
                    label.contains(term.as_str()) || description.contains(term.as_str())
                });
                anywhere.then_some((in_label, row))
            }),
    );
    matches.sort_by(|(left_label, left), (right_label, right)| {
        rank(*left_label, left, *right_label, right)
    });

    let size = usize::try_from(page_size.max(1)).unwrap_or(usize::MAX);
    let total_pages = u32::try_from(matches.len().div_ceil(size))
        .unwrap_or(u32::MAX)
        .max(1);
    let skip = usize::try_from(page.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .saturating_mul(size);

    tracing::debug!(
        query_length = query.len(),
        matches = matches.len(),
        "official search answered from the local index"
    );
    Some(SourcePage {
        servers: matches
            .into_iter()
            .skip(skip)
            .take(size)
            .map(|(_, row)| row)
            .collect(),
        total_pages,
        freshness: RegistryFreshness::Indexed,
    })
}

/// The order of two matches: curated first, then name or title matches, then
/// alphabetical by display name and qualified name.
fn rank(
    left_label: bool,
    left: &RegistryServerSummary,
    right_label: bool,
    right: &RegistryServerSummary,
) -> Order {
    let curated = |row: &RegistryServerSummary| curated_server(&row.qualified_name).is_none();

    curated(left)
        .cmp(&curated(right))
        .then_with(|| right_label.cmp(&left_label))
        .then_with(|| {
            left.display_name
                .to_lowercase()
                .cmp(&right.display_name.to_lowercase())
        })
        .then_with(|| left.qualified_name.cmp(&right.qualified_name))
}

/// The lowercased text a query's words are matched against as the name.
fn search_label(qualified_name: &str, display_name: &str) -> String {
    format!("{qualified_name} {display_name}").to_lowercase()
}

/// The rows a list page adds to the index.
fn index_rows(document: &Value) -> Vec<IndexRow> {
    latest_server_records(document)
        .into_iter()
        .filter_map(|record| {
            let server: OfficialServer = serde_json::from_value(record.clone()).ok()?;
            Some(IndexRow {
                qualified_name: server.name.clone(),
                label: search_label(&server.name, &server.display_name()),
                description: server.description().unwrap_or_default().to_lowercase(),
                record_json: record.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
