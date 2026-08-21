//! The Smithery catalog.
//!
//! `GET /servers?q=&page=&pageSize=` lists; `GET /servers/{qualifiedName}`
//! details. Both are cached in the store.
//!
//! # Trust signals are scrubbed on the way in
//!
//! `website_url` and `auth_kind` decide whether a row passes the strict catalog
//! filter, and they are derived by the *official* adapter from metadata it has
//! checked. A Smithery payload emitting either key must not be able to set
//! them, so they are cleared — on the field *and* in the passthrough bucket,
//! since a value left there would serialize straight back out.

use std::time::Duration;

use crate::error::{Error, Result};
use crate::registry::Store;
use tinymcp_bus::{RegistryListResponse, RegistryServerDetail, RegistryServerSummary};

use super::encode::encode_path_segment;
use super::shared::{MAX_ERROR_BODY_BYTES, cache, truncate};
use super::types::SOURCE_SMITHERY;

/// Where Smithery's registry lives.
const BASE_URL: &str = "https://registry.smithery.ai";

/// How long to wait on Smithery.
const TIMEOUT: Duration = Duration::from_secs(15);

/// The Smithery catalog adapter.
#[derive(Debug)]
pub struct SmitheryRegistry {
    http: reqwest::Client,
}

impl SmitheryRegistry {
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
    /// Returns [`Error::Http`] when Smithery answers with a failure status,
    /// [`Error::Transport`] when it cannot be reached, and
    /// [`Error::MalformedResponse`] when the body is not the expected shape.
    pub(super) async fn search(
        &self,
        store: &Store,
        api_key: Option<&str>,
        query: &str,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<RegistryServerSummary>, u32)> {
        let cache_key = format!("smithery:search:{query}:{page}:{page_size}");

        // A cached body that will not parse is treated as a miss rather than an
        // error: it is almost certainly from an older shape, and refetching is
        // both correct and self-healing.
        if let Ok(Some(cached)) = store.cached(&cache_key)
            && let Ok(parsed) = serde_json::from_str::<RegistryListResponse>(&cached)
        {
            tracing::debug!(cache_key, "smithery search cache hit");
            let total_pages = parsed.pagination.total_pages;
            return Ok((tag_source(parsed.servers), total_pages));
        }

        let url = format!("{BASE_URL}/servers");
        let mut request = self.http.get(&url).header("Accept", "application/json");
        if !query.is_empty() {
            request = request.query(&[("q", query)]);
        }
        request = request.query(&[
            ("page", page.to_string()),
            ("pageSize", page_size.to_string()),
        ]);
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }

        let body = read_body(request, &url).await?;
        let parsed: RegistryListResponse = serde_json::from_str(&body)
            .map_err(|error| Error::malformed(format!("smithery list response: {error}")))?;

        let total_pages = parsed.pagination.total_pages;
        let servers = tag_source(parsed.servers);

        cache(store, &cache_key, &body);
        Ok((servers, total_pages))
    }

    /// Fetches one server's detail.
    ///
    /// # Errors
    ///
    /// As [`Self::search`].
    pub(super) async fn get(
        &self,
        store: &Store,
        api_key: Option<&str>,
        qualified_name: &str,
    ) -> Result<RegistryServerDetail> {
        let cache_key = format!("smithery:detail:{qualified_name}");

        if let Ok(Some(cached)) = store.cached(&cache_key)
            && let Ok(mut detail) = serde_json::from_str::<RegistryServerDetail>(&cached)
        {
            tracing::debug!(qualified_name, "smithery detail cache hit");
            if detail.source.is_empty() {
                detail.source = SOURCE_SMITHERY.to_string();
            }
            return Ok(detail);
        }

        let url = format!("{BASE_URL}/servers/{}", encode_path_segment(qualified_name));
        let mut request = self.http.get(&url).header("Accept", "application/json");
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }

        let body = read_body(request, &url).await?;
        let mut detail: RegistryServerDetail = serde_json::from_str(&body)
            .map_err(|error| Error::malformed(format!("smithery detail response: {error}")))?;
        detail.source = SOURCE_SMITHERY.to_string();

        cache(store, &cache_key, &body);
        Ok(detail)
    }
}

/// Sends a request and returns its body, judging the status first.
async fn read_body(request: reqwest::RequestBuilder, url: &str) -> Result<String> {
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
            // Bounded: an upstream failure body can be a whole error page, and
            // this ends up in a log and an error message.
            body: truncate(&body, MAX_ERROR_BODY_BYTES),
        });
    }

    Ok(body)
}

/// Stamps the source and clears the trust signals. See the module note.
pub(super) fn tag_source(mut servers: Vec<RegistryServerSummary>) -> Vec<RegistryServerSummary> {
    for server in &mut servers {
        if server.source.is_empty() {
            server.source = SOURCE_SMITHERY.to_string();
        }
        server.website_url = None;
        server.auth_kind = None;
        server.extra.remove("website_url");
        server.extra.remove("auth_kind");
    }
    servers
}
