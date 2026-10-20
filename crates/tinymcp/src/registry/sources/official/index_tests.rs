//! Unit tests for the official catalog index.
//!
//! Every registry here is a loopback server serving fixed pages, so a sync is
//! deterministic and nothing reaches the network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::Uri;
use axum::response::IntoResponse as _;
use axum::routing::get;
use serde_json::json;

use super::*;
use crate::registry::Registries;
use crate::registry::sources::official::McpOfficialRegistry;

/// A loopback registry serving fixed pages, with switches for failure.
#[derive(Debug, Default)]
struct Upstream {
    pages: Vec<Vec<Value>>,
    /// The one-based page that answers 503, or zero for none.
    failing_page: AtomicUsize,
    /// Answer every page with a body that is not a list.
    garbled: AtomicBool,
    /// Answer every page with a next cursor naming that same page.
    repeat_cursor: AtomicBool,
    /// How long each listing waits before answering.
    delay: Duration,
    requests: AtomicUsize,
    cursors: Mutex<Vec<Option<String>>>,
    searches: AtomicUsize,
    limits: Mutex<Vec<Option<String>>>,
}

impl Upstream {
    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn cursors(&self) -> Vec<Option<String>> {
        self.cursors.lock().clone()
    }
}

fn param(uri: &Uri, name: &str) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.replace('+', " "))
    })
}

/// An installable envelope for `name`.
fn envelope(name: &str, title: &str, description: &str) -> Value {
    json!({
        "server": {
            "name": name,
            "title": title,
            "description": description,
            "remotes": [{ "url": format!("https://{}.test/mcp", title.to_lowercase()) }],
        },
    })
}

/// Pages of one plain server each, named `io.example/server-N`.
fn numbered_pages(count: usize) -> Vec<Vec<Value>> {
    (1..=count)
        .map(|page| {
            vec![envelope(
                &format!("io.example/server-{page}"),
                &format!("Server {page}"),
                "a server",
            )]
        })
        .collect()
}

async fn serve(upstream: Upstream) -> (String, Arc<Upstream>) {
    let upstream = Arc::new(upstream);
    let app = Router::new()
        .route(
            "/v0/servers",
            get(
                |State(upstream): State<Arc<Upstream>>, uri: Uri| async move {
                    upstream.requests.fetch_add(1, Ordering::SeqCst);
                    if param(&uri, "search").is_some() {
                        upstream.searches.fetch_add(1, Ordering::SeqCst);
                    }
                    let cursor = param(&uri, "cursor");
                    upstream.cursors.lock().push(cursor.clone());
                    upstream.limits.lock().push(param(&uri, "limit"));
                    tokio::time::sleep(upstream.delay).await;

                    if upstream.garbled.load(Ordering::SeqCst) {
                        return "[1, 2, 3]".into_response();
                    }

                    let page: usize = cursor
                        .as_deref()
                        .and_then(|cursor| cursor.parse().ok())
                        .unwrap_or(1);
                    if upstream.failing_page.load(Ordering::SeqCst) == page {
                        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "down")
                            .into_response();
                    }

                    let servers = upstream.pages.get(page - 1).cloned().unwrap_or_default();
                    let mut body = json!({ "servers": servers });
                    if upstream.repeat_cursor.load(Ordering::SeqCst) {
                        body["metadata"] = json!({ "nextCursor": page.to_string() });
                    } else if page < upstream.pages.len() {
                        body["metadata"] = json!({ "nextCursor": (page + 1).to_string() });
                    }
                    axum::Json(body).into_response()
                },
            ),
        )
        .fallback(|| async { (axum::http::StatusCode::NOT_FOUND, "no such server") })
        .with_state(Arc::clone(&upstream));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (base, upstream)
}

fn auth_at(base: &str) -> McpRegistryAuthConfig {
    McpRegistryAuthConfig {
        mcp_official_base: Some(base.to_string()),
        ..McpRegistryAuthConfig::default()
    }
}

fn timeouts(cooldown: Duration) -> RegistryTimeouts {
    RegistryTimeouts {
        browse: Duration::from_secs(5),
        cooldown,
        ..RegistryTimeouts::default()
    }
}

fn index_with(settings: RegistryIndexSettings, cooldown: Duration) -> Arc<OfficialIndex> {
    Arc::new(OfficialIndex::new(
        reqwest::Client::new(),
        timeouts(cooldown),
        settings,
    ))
}

fn index() -> Arc<OfficialIndex> {
    index_with(RegistryIndexSettings::default(), Duration::from_secs(60))
}

fn store() -> Arc<Store> {
    Arc::new(Store::open_in_memory().unwrap())
}

fn indexed_names(store: &Store) -> Vec<String> {
    let mut names: Vec<String> = store
        .search_index(INDEX_SOURCE, &[])
        .unwrap()
        .into_iter()
        .map(|hit| hit.qualified_name)
        .collect();
    names.sort();
    names
}

fn page_names(page: &SourcePage) -> Vec<&str> {
    page.servers
        .iter()
        .map(|server| server.qualified_name.as_str())
        .collect()
}

/// Waits until no sync of `index` is in flight.
async fn settle(index: &OfficialIndex) {
    for _ in 0..500 {
        if !index.in_flight.load(Ordering::Acquire) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the background sync did not finish");
}

/// Moves the last finished sync back by `hours`.
fn age_sync(store: &Store, hours: i64) {
    store.age_collection(
        crate::storage::REGISTRY_INDEX_STATE,
        &["synced_ms"],
        hours * 60 * 60 * 1_000,
    );
}

/// A store whose index holds `pages`, synced from `base`.
async fn synced(base: &str, store: &Store) {
    index().sync(store, &auth_at(base)).await.unwrap();
}

// ---------------------------------------------------------------------------
// Syncing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_sync_walks_every_page_by_cursor() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(3),
        ..Upstream::default()
    })
    .await;
    let store = store();

    let pages = index().sync(&store, &auth_at(&base)).await.unwrap();

    assert_eq!(pages, 3);
    assert_eq!(
        upstream.cursors(),
        [None, Some("2".to_string()), Some("3".to_string())]
    );
    assert!(
        upstream
            .limits
            .lock()
            .iter()
            .all(|limit| limit.as_deref() == Some("100"))
    );
    assert_eq!(upstream.searches.load(Ordering::SeqCst), 0);
    assert_eq!(
        indexed_names(&store),
        [
            "io.example/server-1",
            "io.example/server-2",
            "io.example/server-3"
        ]
    );
    assert!(is_ready(&store, &base));
}

#[tokio::test]
async fn a_sync_keeps_one_row_per_server_preferring_the_latest_version() {
    let mut old = envelope("io.example/versioned", "Old", "old");
    old["_meta"] = json!({
        "io.modelcontextprotocol.registry/official": { "isLatest": false },
    });
    let mut new = envelope("io.example/versioned", "New", "new");
    new["_meta"] = json!({
        "io.modelcontextprotocol.registry/official": { "isLatest": true },
    });
    let mut deprecated = envelope("io.example/deprecated", "Gone", "gone");
    deprecated["_meta"] = json!({
        "io.modelcontextprotocol.registry/official": { "status": "deprecated" },
    });
    let not_installable = json!({ "server": { "name": "io.example/nothing" } });
    let (base, _upstream) = serve(Upstream {
        pages: vec![vec![new, old, deprecated, not_installable, json!({})]],
        ..Upstream::default()
    })
    .await;
    let store = store();

    synced(&base, &store).await;

    assert_eq!(indexed_names(&store), ["io.example/versioned"]);
    let page = search(&store, &base, "versioned", 1, 20).unwrap();
    assert_eq!(page.servers[0].display_name, "New");
}

#[tokio::test]
async fn a_failed_page_keeps_what_was_stored_and_the_next_sync_resumes_there() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(3),
        failing_page: AtomicUsize::new(2),
        ..Upstream::default()
    })
    .await;
    let store = store();
    let index = index();

    let error = index.sync(&store, &auth_at(&base)).await.unwrap_err();
    assert!(error.is_registry_unavailable(), "{error:?}");
    assert_eq!(indexed_names(&store), ["io.example/server-1"]);
    assert!(!is_ready(&store, &base), "a partial copy is not served");
    let state = store.index_state(INDEX_SOURCE).unwrap().unwrap();
    assert_eq!(state.cursor.as_deref(), Some("2"));
    assert_eq!(state.pages, 1);

    upstream.failing_page.store(0, Ordering::SeqCst);
    upstream.cursors.lock().clear();
    let pages = index.sync(&store, &auth_at(&base)).await.unwrap();

    assert_eq!(pages, 3);
    assert_eq!(
        upstream.cursors(),
        [Some("2".to_string()), Some("3".to_string())],
        "the resumed sync does not start over"
    );
    assert_eq!(indexed_names(&store).len(), 3);
    assert!(is_ready(&store, &base));
}

#[tokio::test]
async fn a_sync_resumed_after_its_last_page_was_written_just_finishes() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(1),
        ..Upstream::default()
    })
    .await;
    let store = store();
    store.begin_index_sync(INDEX_SOURCE, &base).unwrap();
    store.store_index_page(INDEX_SOURCE, &[], None).unwrap();

    let pages = index().sync(&store, &auth_at(&base)).await.unwrap();

    assert_eq!(pages, 1);
    assert_eq!(upstream.requests(), 0);
    assert!(is_ready(&store, &base));
}

#[tokio::test]
async fn a_sync_pauses_at_the_page_limit_and_the_next_run_resumes_it() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(5),
        ..Upstream::default()
    })
    .await;
    let store = store();
    let index = index_with(
        RegistryIndexSettings {
            max_pages: 2,
            ..RegistryIndexSettings::default()
        },
        Duration::from_secs(60),
    );

    assert_eq!(index.sync(&store, &auth_at(&base)).await.unwrap(), 2);
    assert_eq!(indexed_names(&store).len(), 2);
    assert!(!is_ready(&store, &base), "a paused sync is not finished");

    assert_eq!(index.sync(&store, &auth_at(&base)).await.unwrap(), 4);
    assert!(!is_ready(&store, &base));

    assert_eq!(index.sync(&store, &auth_at(&base)).await.unwrap(), 5);
    assert_eq!(upstream.requests(), 5, "no page is read twice");
    assert_eq!(indexed_names(&store).len(), 5);
    assert!(is_ready(&store, &base));
}

#[tokio::test]
async fn a_cursor_that_repeats_stops_the_sync_as_malformed() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(3),
        repeat_cursor: AtomicBool::new(true),
        ..Upstream::default()
    })
    .await;
    let store = store();

    let error = index().sync(&store, &auth_at(&base)).await.unwrap_err();

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
    assert_eq!(upstream.requests(), 2);
    assert!(!is_ready(&store, &base));
}

#[tokio::test]
async fn a_refresh_paused_at_the_page_limit_prunes_nothing() {
    let (base, _upstream) = serve(Upstream {
        pages: numbered_pages(3),
        ..Upstream::default()
    })
    .await;
    let store = store();
    synced(&base, &store).await;
    let before = indexed_names(&store).len();
    store.begin_index_sync(INDEX_SOURCE, &base).unwrap();
    let index = index_with(
        RegistryIndexSettings {
            max_pages: 1,
            ..RegistryIndexSettings::default()
        },
        Duration::from_secs(60),
    );

    index.sync(&store, &auth_at(&base)).await.unwrap();

    assert_eq!(indexed_names(&store).len(), before);
}

#[tokio::test]
async fn a_page_that_is_not_a_list_is_malformed_and_writes_nothing() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(1),
        ..Upstream::default()
    })
    .await;
    let store = store();
    synced(&base, &store).await;
    upstream.garbled.store(true, Ordering::SeqCst);

    let error = index().sync(&store, &auth_at(&base)).await.unwrap_err();

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
    assert_eq!(indexed_names(&store), ["io.example/server-1"]);
}

#[tokio::test]
async fn a_body_that_is_not_json_is_malformed() {
    let app = Router::new().fallback(|| async { "not json" });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let error = index().sync(&store(), &auth_at(&base)).await.unwrap_err();

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_configured_token_is_sent_with_every_sync_page() {
    let seen = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let app = Router::new()
        .route(
            "/v0/servers",
            get(
                |State(seen): State<Arc<Mutex<Vec<Option<String>>>>>,
                 headers: axum::http::HeaderMap| async move {
                    seen.lock().push(
                        headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .map(ToString::to_string),
                    );
                    axum::Json(json!({ "servers": [] }))
                },
            ),
        )
        .with_state(Arc::clone(&seen));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let auth = McpRegistryAuthConfig {
        mcp_official_token: Some("secret".to_string()),
        ..auth_at(&base)
    };

    index().sync(&store(), &auth).await.unwrap();

    assert_eq!(seen.lock().as_slice(), [Some("Bearer secret".to_string())]);
}

// ---------------------------------------------------------------------------
// When a sync is due
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_sync_is_due_until_one_finishes_and_again_after_the_refresh_interval() {
    let (base, _upstream) = serve(Upstream {
        pages: numbered_pages(1),
        ..Upstream::default()
    })
    .await;
    let store = store();
    let index = index();

    assert!(index.is_due(&store, &base), "no index yet");
    index.sync(&store, &auth_at(&base)).await.unwrap();
    assert!(!index.is_due(&store, &base), "just synced");

    age_sync(&store, 5);
    assert!(!index.is_due(&store, &base), "inside the six hours");
    age_sync(&store, 2);
    assert!(index.is_due(&store, &base), "past the six hours");
}

#[tokio::test]
async fn a_sync_is_due_for_a_different_catalog_or_an_unfinished_one() {
    let (base, _upstream) = serve(Upstream {
        pages: numbered_pages(1),
        ..Upstream::default()
    })
    .await;
    let store = store();
    let index = index();
    index.sync(&store, &auth_at(&base)).await.unwrap();

    assert!(index.is_due(&store, "https://elsewhere.test"));

    store.begin_index_sync(INDEX_SOURCE, &base).unwrap();
    assert!(index.is_due(&store, &base));
}

#[tokio::test]
async fn a_refresh_syncs_in_the_background_and_then_is_not_due() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(2),
        ..Upstream::default()
    })
    .await;
    let store = store();
    let index = index();

    assert!(index.refresh(&store, &auth_at(&base)));
    settle(&index).await;

    assert!(is_ready(&store, &base));
    assert_eq!(upstream.requests(), 2);
    assert!(!index.refresh(&store, &auth_at(&base)));
    assert_eq!(upstream.requests(), 2);
}

#[tokio::test]
async fn only_one_sync_is_ever_in_flight() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(2),
        delay: Duration::from_millis(200),
        ..Upstream::default()
    })
    .await;
    let store = store();
    let index = index();

    let started: Vec<bool> = (0..5)
        .map(|_| index.refresh(&store, &auth_at(&base)))
        .collect();
    assert_eq!(started, [true, false, false, false, false]);

    settle(&index).await;
    assert_eq!(upstream.requests(), 2, "one walk of two pages");
    assert!(is_ready(&store, &base));
}

#[tokio::test]
async fn a_failed_background_sync_waits_out_the_cooldown_before_resuming() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(2),
        failing_page: AtomicUsize::new(2),
        ..Upstream::default()
    })
    .await;
    let store = store();
    let index = index();

    assert!(index.refresh(&store, &auth_at(&base)));
    settle(&index).await;
    assert!(!is_ready(&store, &base));

    upstream.failing_page.store(0, Ordering::SeqCst);
    assert!(
        !index.refresh(&store, &auth_at(&base)),
        "still cooling down"
    );
    *index.retry_at.lock() = Some(Instant::now());

    assert!(index.refresh(&store, &auth_at(&base)));
    settle(&index).await;
    assert!(is_ready(&store, &base));
    assert_eq!(index.retry_at.lock().as_ref(), None);
}

#[test]
fn a_refresh_outside_a_runtime_starts_nothing() {
    let store = store();
    let index = index();

    assert!(!index.refresh(&store, &auth_at("http://127.0.0.1:9")));
    assert!(!index.in_flight.load(Ordering::Acquire));
}

#[tokio::test]
async fn a_refresh_through_the_dispatcher_syncs_the_configured_catalog() {
    let (base, _upstream) = serve(Upstream {
        pages: numbered_pages(1),
        ..Upstream::default()
    })
    .await;
    let store = store();
    let registries = Registries::with_official(
        auth_at(&base),
        McpOfficialRegistry::with_settings(
            timeouts(Duration::from_secs(60)),
            RegistryIndexSettings::default(),
        )
        .unwrap(),
    )
    .unwrap();

    assert!(registries.refresh_index(&store));
    for _ in 0..500 {
        if is_ready(&store, &base) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(is_ready(&store, &base));
}

// ---------------------------------------------------------------------------
// Searching
// ---------------------------------------------------------------------------

/// A store whose index holds the given envelopes on one page.
async fn indexed(servers: Vec<Value>) -> (String, Arc<Upstream>, Arc<Store>) {
    let (base, upstream) = serve(Upstream {
        pages: vec![servers],
        ..Upstream::default()
    })
    .await;
    let store = store();
    synced(&base, &store).await;
    (base, upstream, store)
}

#[tokio::test]
async fn an_index_that_has_not_finished_a_sync_is_not_searched() {
    let store = store();
    store
        .begin_index_sync(INDEX_SOURCE, "https://registry.test")
        .unwrap();

    assert!(search(&store, "https://registry.test", "slack", 1, 20).is_none());
}

#[tokio::test]
async fn an_index_of_a_different_catalog_is_not_searched() {
    let (_base, _upstream, store) = indexed(vec![envelope("io.example/a", "A", "")]).await;

    assert!(search(&store, "https://elsewhere.test", "a", 1, 20).is_none());
}

#[tokio::test]
async fn a_query_with_no_words_is_not_searched_locally() {
    let (base, _upstream, store) = indexed(vec![envelope("io.example/a", "A", "")]).await;

    assert!(search(&store, &base, "   ", 1, 20).is_none());
}

#[tokio::test]
async fn a_search_ranks_curated_then_name_then_description_then_alphabetical() {
    let (base, _upstream, store) = indexed(vec![
        envelope("io.example/zeta", "Zeta Slack", "posts messages"),
        envelope("io.example/alpha", "Alpha Chat", "bridges slack and email"),
        envelope("io.example/beta", "Beta Slack", "reads channels"),
        envelope("io.example/mail", "Mail", "sends email"),
    ])
    .await;

    let page = search(&store, &base, "Slack", 1, 20).unwrap();

    assert_eq!(page.freshness, RegistryFreshness::Indexed);
    assert_eq!(
        page_names(&page),
        [
            "com.slack/mcp",
            "io.example/beta",
            "io.example/zeta",
            "io.example/alpha"
        ]
    );
    assert_eq!(page.total_pages, 1);
}

#[tokio::test]
async fn every_word_of_the_query_must_match() {
    let (base, _upstream, store) = indexed(vec![
        envelope("io.example/zeta", "Zeta Slack", "posts messages"),
        envelope("io.example/beta", "Beta Slack", "reads channels"),
    ])
    .await;

    let page = search(&store, &base, "slack posts", 1, 20).unwrap();

    assert_eq!(page_names(&page), ["io.example/zeta"]);
}

#[tokio::test]
async fn a_curated_server_the_index_holds_appears_once_with_the_registry_record() {
    let (base, _upstream, store) = indexed(vec![
        envelope("io.example/notion-sync", "Notion Sync", "a notion helper"),
        envelope("com.notion/mcp", "Notion", "from the registry"),
    ])
    .await;

    let page = search(&store, &base, "notion", 1, 20).unwrap();

    assert_eq!(
        page_names(&page),
        ["com.notion/mcp", "io.example/notion-sync"]
    );
    assert_eq!(
        page.servers[0].description.as_deref(),
        Some("from the registry")
    );
}

#[tokio::test]
async fn a_curated_server_absent_from_the_index_matches_on_its_description() {
    let (base, _upstream, store) = indexed(vec![envelope("io.example/a", "A", "")]).await;

    let page = search(&store, &base, "jira confluence", 1, 20).unwrap();

    assert_eq!(page_names(&page), ["com.atlassian/atlassian-mcp-server"]);
    assert_eq!(page.servers[0].source, SOURCE_MCP_OFFICIAL);
}

#[tokio::test]
async fn a_search_pages_through_its_matches() {
    let servers = (1..=5)
        .map(|n| {
            envelope(
                &format!("io.example/widget-{n}"),
                &format!("Widget {n}"),
                "",
            )
        })
        .collect();
    let (base, _upstream, store) = indexed(servers).await;

    let first = search(&store, &base, "widget", 1, 2).unwrap();
    let last = search(&store, &base, "widget", 3, 2).unwrap();
    let beyond = search(&store, &base, "widget", 4, 2).unwrap();

    assert_eq!(
        page_names(&first),
        ["io.example/widget-1", "io.example/widget-2"]
    );
    assert_eq!(first.total_pages, 3);
    assert_eq!(page_names(&last), ["io.example/widget-5"]);
    assert_eq!(page_names(&beyond), Vec::<&str>::new());
    assert_eq!(beyond.total_pages, 3);
}

#[tokio::test]
async fn a_search_matching_nothing_is_an_empty_indexed_page() {
    let (base, _upstream, store) = indexed(vec![envelope("io.example/a", "A", "")]).await;

    let page = search(&store, &base, "nothing-matches-this", 1, 20).unwrap();

    assert_eq!(page_names(&page), Vec::<&str>::new());
    assert_eq!(page.total_pages, 1);
    assert_eq!(page.freshness, RegistryFreshness::Indexed);
}

// ---------------------------------------------------------------------------
// Through the adapter
// ---------------------------------------------------------------------------

fn adapter() -> McpOfficialRegistry {
    McpOfficialRegistry::with_settings(
        timeouts(Duration::from_secs(60)),
        RegistryIndexSettings::default(),
    )
    .unwrap()
}

fn cursors() -> Mutex<std::collections::HashMap<(String, u32, u32), String>> {
    Mutex::new(std::collections::HashMap::new())
}

#[tokio::test]
async fn the_adapter_answers_a_query_from_the_index_without_asking_the_registry() {
    let (base, upstream, store) =
        indexed(vec![envelope("io.example/slack-bot", "Slack Bot", "")]).await;
    let before = upstream.requests();

    let page = adapter()
        .search(&store, &auth_at(&base), &cursors(), "slack", 1, 20)
        .await
        .unwrap();

    assert_eq!(page.freshness, RegistryFreshness::Indexed);
    assert_eq!(page_names(&page), ["com.slack/mcp", "io.example/slack-bot"]);
    assert_eq!(upstream.requests(), before);
    assert_eq!(upstream.searches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_adapter_still_browses_the_registry_with_an_index() {
    let (base, upstream, store) = indexed(vec![envelope("io.example/a", "A", "")]).await;
    let before = upstream.requests();

    let page = adapter()
        .search(&store, &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(page.freshness, RegistryFreshness::Live);
    assert_eq!(upstream.requests(), before + 1);
}

#[tokio::test]
async fn without_an_index_the_adapter_asks_the_registry_to_search() {
    let (base, upstream) = serve(Upstream {
        pages: numbered_pages(1),
        ..Upstream::default()
    })
    .await;

    let page = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "server", 1, 20)
        .await
        .unwrap();

    assert_eq!(page.freshness, RegistryFreshness::Live);
    assert_eq!(upstream.searches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_curated_server_the_registry_does_not_list_is_described_from_its_entry() {
    let (base, _upstream) = serve(Upstream::default()).await;

    let detail = adapter()
        .get(&store(), &auth_at(&base), "com.slack/mcp")
        .await
        .unwrap();

    assert_eq!(detail.qualified_name, "com.slack/mcp");
    assert_eq!(
        detail.connections[0].deployment_url.as_deref(),
        Some("https://mcp.slack.com/mcp")
    );
}

#[tokio::test]
async fn an_uncurated_server_the_registry_does_not_list_is_still_an_error() {
    let (base, _upstream) = serve(Upstream::default()).await;

    let error = adapter()
        .get(&store(), &auth_at(&base), "io.example/nowhere")
        .await
        .unwrap_err();

    assert!(
        matches!(error, Error::Http { status: 404, .. }),
        "{error:?}"
    );
}
