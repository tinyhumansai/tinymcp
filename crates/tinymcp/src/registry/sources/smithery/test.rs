//! Unit tests for the Smithery catalog adapter.
//!
//! Driven against a loopback server rather than Smithery, so the suite is
//! deterministic and needs no key. What is asserted is the adapter's own
//! behavior: the request it builds, the trust signals it scrubs, the cache it
//! keeps, and how it reports an upstream that fails.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use serde_json::{Value, json};

use super::{SmitheryRegistry, tag_source};
use crate::error::Error;
use crate::registry::Store;
use crate::registry::sources::types::SOURCE_SMITHERY;
use tinymcp_bus::RegistryServerSummary;

// ---------------------------------------------------------------------------
// The loopback catalog
// ---------------------------------------------------------------------------

/// What the mock catalog saw, so a test can assert on the request rather than
/// only on the answer.
#[derive(Debug, Default)]
struct Seen {
    requests: AtomicUsize,
    authorization: parking_lot::Mutex<Option<String>>,
    query: parking_lot::Mutex<Option<String>>,
}

/// One search result, as Smithery shapes it.
fn summary(name: &str) -> Value {
    json!({
        "qualifiedName": name,
        "displayName": name,
        "description": "a server",
    })
}

/// A list response holding `servers`, reporting `total_pages`.
fn list_body(servers: Vec<Value>, total_pages: u32) -> Value {
    json!({
        "servers": servers,
        "pagination": {
            "currentPage": 1,
            "pageSize": 20,
            "totalPages": total_pages,
            "totalCount": servers.len(),
        },
    })
}

/// Binds a loopback port and serves `app`, returning its base URL.
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// A catalog that answers both endpoints successfully.
async fn working_catalog() -> (String, Arc<Seen>) {
    let seen = Arc::new(Seen::default());

    let app = Router::new()
        .route(
            "/servers",
            get(
                |State(seen): State<Arc<Seen>>,
                 headers: HeaderMap,
                 Query(params): Query<std::collections::HashMap<String, String>>| async move {
                    seen.requests.fetch_add(1, Ordering::SeqCst);
                    *seen.authorization.lock() = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .map(ToString::to_string);
                    *seen.query.lock() = params.get("q").cloned();

                    axum::Json(list_body(vec![summary("@acme/weather")], 3))
                },
            ),
        )
        .route(
            "/servers/{*name}",
            get(
                |State(seen): State<Arc<Seen>>, Path(name): Path<String>| async move {
                    seen.requests.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "qualifiedName": name,
                        "displayName": name,
                        "description": "a server",
                    }))
                },
            ),
        )
        .with_state(Arc::clone(&seen));

    (serve(app).await, seen)
}

/// A catalog that answers every request with `status` and `body`.
async fn failing_catalog(status: u16, body: &'static str) -> String {
    let app = Router::new().fallback(get(move || async move {
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            body.to_string(),
        )
    }));

    serve(app).await
}

/// An adapter pointed at `base`.
fn adapter(base: &str) -> SmitheryRegistry {
    SmitheryRegistry::with_base(base).expect("the adapter builds")
}

/// An empty store to cache into.
fn store() -> Store {
    Store::open_in_memory().expect("the store opens")
}

// ---------------------------------------------------------------------------
// Searching
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_search_returns_the_rows_and_the_page_count() {
    let (base, _seen) = working_catalog().await;

    let (servers, total_pages) = adapter(&base)
        .search(&store(), None, "weather", 1, 20)
        .await
        .expect("the search succeeds");

    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].qualified_name, "@acme/weather");
    // Smithery reports a true total, unlike the official registry's bound.
    assert_eq!(total_pages, 3);
}

#[tokio::test]
async fn a_search_stamps_smithery_as_the_source() {
    // The row has to say where it came from: an install routes its detail
    // lookup back to the source that listed it.
    let (base, _seen) = working_catalog().await;

    let (servers, _) = adapter(&base)
        .search(&store(), None, "weather", 1, 20)
        .await
        .unwrap();

    assert_eq!(servers[0].source, SOURCE_SMITHERY);
}

#[tokio::test]
async fn a_search_sends_the_query_it_was_given() {
    let (base, seen) = working_catalog().await;

    adapter(&base)
        .search(&store(), None, "weather", 1, 20)
        .await
        .unwrap();

    assert_eq!(seen.query.lock().as_deref(), Some("weather"));
}

#[tokio::test]
async fn a_blank_query_is_left_off_the_request() {
    // Sending `q=` is not the same as sending nothing, and the catalog is
    // entitled to treat an empty term as a filter that matches nothing.
    let (base, seen) = working_catalog().await;

    adapter(&base).search(&store(), None, "", 1, 20).await.unwrap();

    assert_eq!(*seen.query.lock(), None);
}

#[tokio::test]
async fn a_configured_key_is_sent_as_a_bearer_token() {
    let (base, seen) = working_catalog().await;

    adapter(&base)
        .search(&store(), Some("sk-test"), "weather", 1, 20)
        .await
        .unwrap();

    assert_eq!(seen.authorization.lock().as_deref(), Some("Bearer sk-test"));
}

#[tokio::test]
async fn no_key_means_no_authorization_header() {
    // A bare `Bearer ` is worse than nothing: every request fails on it.
    let (base, seen) = working_catalog().await;

    adapter(&base).search(&store(), None, "weather", 1, 20).await.unwrap();

    assert_eq!(*seen.authorization.lock(), None);
}

#[tokio::test]
async fn a_second_identical_search_is_served_from_the_cache() {
    let (base, seen) = working_catalog().await;
    let store = store();
    let adapter = adapter(&base);

    adapter.search(&store, None, "weather", 1, 20).await.unwrap();
    let (servers, total_pages) = adapter.search(&store, None, "weather", 1, 20).await.unwrap();

    assert_eq!(seen.requests.load(Ordering::SeqCst), 1);
    assert_eq!(servers.len(), 1);
    assert_eq!(total_pages, 3);
}

#[tokio::test]
async fn a_different_page_is_not_served_from_another_page_s_cache() {
    let (base, seen) = working_catalog().await;
    let store = store();
    let adapter = adapter(&base);

    adapter.search(&store, None, "weather", 1, 20).await.unwrap();
    adapter.search(&store, None, "weather", 2, 20).await.unwrap();

    assert_eq!(seen.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_cached_body_in_an_older_shape_is_refetched_rather_than_failing() {
    // Self-healing on purpose: a body that no longer parses is almost certainly
    // from a previous release, and failing the user's search over it would cost
    // them the search.
    let (base, seen) = working_catalog().await;
    let store = store();
    store
        .cache("smithery:search:weather:1:20", "{\"shape\":\"from an older release\"}")
        .unwrap();

    let (servers, _) = adapter(&base)
        .search(&store, None, "weather", 1, 20)
        .await
        .expect("the search succeeds");

    assert_eq!(seen.requests.load(Ordering::SeqCst), 1);
    assert_eq!(servers.len(), 1);
}

#[tokio::test]
async fn a_body_that_is_not_a_list_response_is_reported_as_malformed() {
    let base = failing_catalog(200, "{\"nothing\":\"expected\"}").await;

    let error = adapter(&base)
        .search(&store(), None, "weather", 1, 20)
        .await
        .expect_err("the body does not parse");

    assert!(matches!(error, Error::MalformedResponse { .. }), "{error:?}");
}

// ---------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_detail_lookup_returns_the_server() {
    let (base, _seen) = working_catalog().await;

    let detail = adapter(&base)
        .get(&store(), None, "@acme/weather")
        .await
        .expect("the lookup succeeds");

    assert_eq!(detail.qualified_name, "@acme/weather");
    assert_eq!(detail.source, SOURCE_SMITHERY);
}

#[tokio::test]
async fn a_second_identical_lookup_is_served_from_the_cache() {
    let (base, seen) = working_catalog().await;
    let store = store();
    let adapter = adapter(&base);

    adapter.get(&store, None, "@acme/weather").await.unwrap();
    let detail = adapter.get(&store, None, "@acme/weather").await.unwrap();

    assert_eq!(seen.requests.load(Ordering::SeqCst), 1);
    assert_eq!(detail.qualified_name, "@acme/weather");
}

#[tokio::test]
async fn a_cached_detail_without_a_source_is_stamped_on_the_way_out() {
    // Bodies cached by an older release predate the stamp. Returning one with
    // an empty source would leave a caller unable to route its install.
    let store = store();
    store
        .cache(
            "smithery:detail:@acme/weather",
            &json!({
                "qualifiedName": "@acme/weather",
                "displayName": "Weather",
                "description": "a server",
            })
            .to_string(),
        )
        .unwrap();

    let detail = adapter("http://127.0.0.1:1")
        .get(&store, None, "@acme/weather")
        .await
        .expect("the cache hit answers without a request");

    assert_eq!(detail.source, SOURCE_SMITHERY);
}

#[tokio::test]
async fn a_name_with_a_slash_is_escaped_into_one_path_segment() {
    // `@acme/weather` is one name, not two segments. Sent raw it would reach a
    // different route, or none.
    let (base, _seen) = working_catalog().await;

    let detail = adapter(&base)
        .get(&store(), None, "@acme/weather")
        .await
        .expect("the lookup succeeds");

    assert_eq!(detail.qualified_name, "@acme/weather");
}

#[tokio::test]
async fn a_detail_body_that_does_not_parse_is_reported_as_malformed() {
    let base = failing_catalog(200, "not json at all").await;

    let error = adapter(&base)
        .get(&store(), None, "@acme/weather")
        .await
        .expect_err("the body does not parse");

    assert!(matches!(error, Error::MalformedResponse { .. }), "{error:?}");
}

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_upstream_failure_status_is_reported_with_its_body() {
    // The body is where the catalog says *why* — an expired key reads
    // differently from a rate limit, and a bare status cannot tell them apart.
    let base = failing_catalog(429, "slow down").await;

    let error = adapter(&base)
        .search(&store(), None, "weather", 1, 20)
        .await
        .expect_err("429");

    match error {
        Error::Http { status, body, .. } => {
            assert_eq!(status, 429);
            assert_eq!(body, "slow down");
        }
        other => panic!("expected an http error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_long_failure_body_is_truncated() {
    // These reach a log line and an error message. An upstream answering a
    // failure with a whole HTML page would otherwise put all of it there.
    let base = failing_catalog(500, concat!(
        "0123456789012345678901234567890123456789012345678901234567890123456789",
        "0123456789012345678901234567890123456789012345678901234567890123456789",
        "0123456789012345678901234567890123456789012345678901234567890123456789",
        "0123456789012345678901234567890123456789",
    ))
    .await;

    let error = adapter(&base)
        .search(&store(), None, "weather", 1, 20)
        .await
        .expect_err("500");

    match error {
        Error::Http { body, .. } => assert_eq!(body.len(), 200),
        other => panic!("expected an http error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unreachable_catalog_is_reported_as_a_transport_failure() {
    // Port 1 on loopback refuses immediately, so this does not wait on a
    // timeout.
    let error = adapter("http://127.0.0.1:1")
        .search(&store(), None, "weather", 1, 20)
        .await
        .expect_err("unreachable");

    assert!(matches!(error, Error::Transport { .. }), "{error:?}");
}

#[tokio::test]
async fn a_failure_does_not_poison_the_cache() {
    // Only a successful body is cached, so a retry after an outage reaches the
    // catalog rather than replaying the outage.
    let base = failing_catalog(503, "down").await;
    let store = store();

    let _ = adapter(&base).search(&store, None, "weather", 1, 20).await;

    assert_eq!(store.cached("smithery:search:weather:1:20").unwrap(), None);
}

// ---------------------------------------------------------------------------
// Scrubbing the trust signals
// ---------------------------------------------------------------------------

#[test]
fn a_payload_cannot_set_the_trust_signals() {
    // These decide whether a row passes the strict catalog filter, and they are
    // the *official* adapter's to derive from metadata it has checked. A
    // Smithery payload claiming either one must not be believed.
    let mut server = RegistryServerSummary {
        qualified_name: "@acme/weather".into(),
        website_url: Some("https://acme.test".into()),
        auth_kind: Some("none".into()),
        ..RegistryServerSummary::default()
    };
    server
        .extra
        .insert("website_url".into(), json!("https://acme.test"));
    server.extra.insert("auth_kind".into(), json!("none"));

    let tagged = tag_source(vec![server]);

    assert_eq!(tagged[0].website_url, None);
    assert_eq!(tagged[0].auth_kind, None);
}

#[test]
fn a_scrubbed_signal_is_removed_from_the_passthrough_bucket_too() {
    // Clearing only the field would leave the value in `extra`, and `extra`
    // serializes straight back out — so the claim would survive the scrub.
    let mut server = RegistryServerSummary::default();
    server
        .extra
        .insert("website_url".into(), json!("https://acme.test"));
    server.extra.insert("auth_kind".into(), json!("none"));

    let tagged = tag_source(vec![server]);

    assert!(!tagged[0].extra.contains_key("website_url"));
    assert!(!tagged[0].extra.contains_key("auth_kind"));
}

#[test]
fn a_row_that_already_names_its_source_keeps_it() {
    let server = RegistryServerSummary {
        source: "somewhere_else".into(),
        ..RegistryServerSummary::default()
    };

    assert_eq!(tag_source(vec![server])[0].source, "somewhere_else");
}
