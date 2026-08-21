//! Unit tests for the official registry adapter.
//!
//! Most of this is shape conversion, and the shapes come from a registry whose
//! schema moves. The tests are built from JSON fixtures rather than from
//! constructed Rust values for exactly that reason: a fixture that no longer
//! parses is the failure worth catching, and a constructed value can never
//! reproduce it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};

use super::types::{OfficialListResponse, OfficialServer};
use super::{page_bound, search_cache_key};

/// A list response wrapping `servers`.
fn list_response(servers: &Value, next_cursor: Option<&str>) -> OfficialListResponse {
    let mut document = json!({ "servers": servers });
    if let Some(cursor) = next_cursor {
        document["metadata"] = json!({ "nextCursor": cursor });
    }
    serde_json::from_value(document).expect("a list response decodes")
}

/// One envelope around a server that can be installed.
fn envelope(name: &str) -> Value {
    json!({
        "server": {
            "name": name,
            "description": "a server",
            "packages": [{ "registryType": "npm", "identifier": name }],
        },
    })
}

// ---------------------------------------------------------------------------
// The envelope
// ---------------------------------------------------------------------------

#[test]
fn a_row_is_read_from_inside_its_envelope() {
    // The bug this guards: an earlier adapter parsed the inner shape at the top
    // level, so serde filled every field with a default and the catalog
    // rendered pages of blank cards.
    let response = list_response(&json!([envelope("io.github.example/server")]), None);
    let summaries = response.into_summaries();

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].qualified_name, "io.github.example/server");
    assert_eq!(summaries[0].description.as_deref(), Some("a server"));
}

#[test]
fn a_row_with_no_server_key_is_a_parse_error_rather_than_a_blank_card() {
    // Everything else here is permissive so a schema bump does not break the
    // catalog. This one must be loud, or the blank-card failure returns looking
    // like an empty registry.
    let decoded = serde_json::from_value::<OfficialListResponse>(json!({
        "servers": [{ "name": "flat-shape", "description": "wrong level" }],
    }));

    assert!(decoded.is_err(), "a flat row should not decode");
}

#[test]
fn an_empty_response_yields_no_rows() {
    assert!(list_response(&json!([]), None).into_summaries().is_empty());
    let empty: OfficialListResponse = serde_json::from_value(json!({})).unwrap();
    assert!(empty.into_summaries().is_empty());
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

#[test]
fn a_row_offering_no_way_to_connect_is_dropped() {
    // A user can only discover such a row is a dead end by trying to install
    // it.
    let response = list_response(
        &json!([{ "server": { "name": "nothing/here", "description": "no way in" } }]),
        None,
    );

    assert!(response.into_summaries().is_empty());
}

#[test]
fn a_row_with_only_a_remote_is_kept() {
    let response = list_response(
        &json!([{
            "server": {
                "name": "remote/only",
                "remotes": [{ "url": "https://api.test/mcp" }],
            },
        }]),
        None,
    );

    let summaries = response.into_summaries();
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].is_deployed, "a hosted remote is deployed");
}

#[test]
fn a_deprecated_row_is_dropped() {
    let response = list_response(
        &json!([{
            "server": {
                "name": "old/server",
                "packages": [{ "registryType": "npm", "identifier": "old" }],
            },
            "_meta": {
                "io.modelcontextprotocol.registry/official": { "status": "deprecated" },
            },
        }]),
        None,
    );

    assert!(response.into_summaries().is_empty());
}

#[test]
fn a_row_with_no_metadata_is_not_treated_as_deprecated() {
    // That is what a row cached by an older build looks like.
    let response = list_response(&json!([envelope("io.github.example/server")]), None);
    assert_eq!(response.into_summaries().len(), 1);
}

#[test]
fn a_row_with_an_active_status_is_kept() {
    let response = list_response(
        &json!([{
            "server": {
                "name": "live/server",
                "packages": [{ "registryType": "npm", "identifier": "live" }],
            },
            "_meta": {
                "io.modelcontextprotocol.registry/official": { "status": "active" },
            },
        }]),
        None,
    );

    assert_eq!(response.into_summaries().len(), 1);
}

#[test]
fn a_repeated_name_appears_once() {
    let response = list_response(&json!([envelope("same/name"), envelope("same/name")]), None);

    assert_eq!(response.into_summaries().len(), 1);
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

#[test]
fn a_declared_title_is_used_as_the_display_name() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "io.github.example/server-bar",
        "title": "Example Server",
    }))
    .unwrap();

    assert_eq!(server.display_name(), "Example Server");
}

#[test]
fn a_blank_title_falls_back_to_the_derived_name() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "io.github.example/server-bar",
        "title": "   ",
    }))
    .unwrap();

    assert_eq!(server.display_name(), "server bar");
}

#[test]
fn a_name_is_derived_from_its_last_segment_with_separators_spaced() {
    // Better to show than `io.github.someone/some-server`.
    for (name, expected) in [
        ("io.github.example/server-bar", "server bar"),
        ("io.github.example/server_bar", "server bar"),
        ("com.vendor.product", "product"),
        ("bare", "bare"),
    ] {
        let server: OfficialServer = serde_json::from_value(json!({ "name": name })).unwrap();
        assert_eq!(server.display_name(), expected, "for {name}");
    }
}

// ---------------------------------------------------------------------------
// Trust signals
// ---------------------------------------------------------------------------

#[test]
fn a_declared_website_becomes_the_trust_signal() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "websiteUrl": "https://vendor.test",
    }))
    .unwrap();

    assert_eq!(
        server.into_summary().website_url.as_deref(),
        Some("https://vendor.test")
    );
}

#[test]
fn a_blank_website_is_not_a_trust_signal() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "websiteUrl": "   ",
    }))
    .unwrap();

    assert_eq!(server.into_summary().website_url, None);
}

#[test]
fn a_secret_header_declares_a_static_credential() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{
            "url": "https://api.test/mcp",
            "headers": [{ "name": "X-Api-Key", "isSecret": true }],
        }],
    }))
    .unwrap();

    assert_eq!(server.into_summary().auth_kind.as_deref(), Some("api_key"));
}

#[test]
fn an_authorization_header_declares_a_static_credential_even_unmarked() {
    // Registries are inconsistent about marking it, and an `Authorization`
    // header is a credential whatever else it says.
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{
            "url": "https://api.test/mcp",
            "headers": [{ "name": "authorization" }],
        }],
    }))
    .unwrap();

    assert_eq!(server.into_summary().auth_kind.as_deref(), Some("api_key"));
}

#[test]
fn a_secret_environment_variable_declares_a_static_credential() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "npm",
            "identifier": "server",
            "environmentVariables": [{ "name": "API_KEY", "isSecret": true }],
        }],
    }))
    .unwrap();

    assert_eq!(server.into_summary().auth_kind.as_deref(), Some("api_key"));
}

#[test]
fn a_server_declaring_nothing_secret_has_no_credential_kind() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/open",
        "remotes": [{ "url": "https://api.test/mcp" }],
    }))
    .unwrap();

    assert_eq!(server.into_summary().auth_kind, None);
}

#[test]
fn a_row_is_never_badged_by_the_adapter() {
    // Badging is curation's job, from its own list.
    let server: OfficialServer =
        serde_json::from_value(json!({ "name": "com.notion/mcp" })).unwrap();

    assert!(!server.into_summary().official);
}

// ---------------------------------------------------------------------------
// Detail conversion
// ---------------------------------------------------------------------------

#[test]
fn a_remote_becomes_an_http_connection_with_its_endpoint() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{ "url": "https://api.test/mcp" }],
    }))
    .unwrap();

    let detail = server.into_detail();
    assert_eq!(detail.connections.len(), 1);
    assert_eq!(detail.connections[0].r#type, "http");
    assert_eq!(
        detail.connections[0].deployment_url.as_deref(),
        Some("https://api.test/mcp")
    );
}

#[test]
fn a_package_becomes_a_subprocess_connection_with_no_endpoint() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{ "registryType": "npm", "identifier": "some-server" }],
    }))
    .unwrap();

    let detail = server.into_detail();
    assert_eq!(detail.connections.len(), 1);
    assert_eq!(detail.connections[0].r#type, "stdio");
    assert_eq!(detail.connections[0].deployment_url, None);
}

#[test]
fn a_server_offering_both_gets_a_connection_for_each() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{ "url": "https://api.test/mcp" }],
        "packages": [{ "registryType": "npm", "identifier": "some-server" }],
    }))
    .unwrap();

    assert_eq!(server.into_detail().connections.len(), 2);
}

// ---------------------------------------------------------------------------
// Input schemas
// ---------------------------------------------------------------------------

#[test]
fn declared_headers_become_an_input_schema() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{
            "url": "https://api.test/mcp",
            "headers": [
                { "name": "X-Api-Key", "description": "your key", "isSecret": true, "isRequired": true },
                { "name": "X-Org", "description": "your organisation" },
            ],
        }],
    }))
    .unwrap();

    let schema = server.into_detail().connections[0]
        .config_schema
        .clone()
        .expect("a schema");

    assert_eq!(
        schema["properties"]["X-Api-Key"]["description"],
        json!("your key")
    );
    assert_eq!(schema["properties"]["X-Api-Key"]["x-secret"], json!(true));
    assert_eq!(schema["required"], json!(["X-Api-Key"]));
    // Not marked secret, so no masking marker.
    assert!(schema["properties"]["X-Org"].get("x-secret").is_none());
}

#[test]
fn a_remote_declaring_no_headers_has_no_schema() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{ "url": "https://api.test/mcp" }],
    }))
    .unwrap();

    assert_eq!(server.into_detail().connections[0].config_schema, None);
}

#[test]
fn an_unnamed_input_is_skipped() {
    // It could neither be prompted for nor sent.
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{
            "url": "https://api.test/mcp",
            "headers": [{ "name": "", "isRequired": true }],
        }],
    }))
    .unwrap();

    assert_eq!(server.into_detail().connections[0].config_schema, None);
}

#[test]
fn declared_environment_variables_become_an_input_schema() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "npm",
            "identifier": "server",
            "environmentVariables": [
                { "name": "API_KEY", "isSecret": true, "isRequired": true },
            ],
        }],
    }))
    .unwrap();

    let schema = server.into_detail().connections[0]
        .config_schema
        .clone()
        .expect("a schema");

    assert_eq!(schema["properties"]["API_KEY"]["x-secret"], json!(true));
    assert_eq!(schema["required"], json!(["API_KEY"]));
}

#[test]
fn a_registry_supplied_schema_is_used_when_no_variables_are_declared() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "npm",
            "identifier": "server",
            "configSchema": { "properties": { "region": {} } },
        }],
    }))
    .unwrap();

    let schema = server.into_detail().connections[0]
        .config_schema
        .clone()
        .expect("a schema");

    assert!(schema["properties"].get("region").is_some());
}

// ---------------------------------------------------------------------------
// Launch examples
// ---------------------------------------------------------------------------

#[test]
fn a_python_package_is_launched_with_uvx() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{ "registryType": "pypi", "identifier": "some-server" }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["command"], json!("uvx"));
    assert_eq!(example["args"], json!(["some-server"]));
}

#[test]
fn a_node_package_is_launched_with_npx_and_the_yes_flag() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{ "registryType": "npm", "identifier": "some-server" }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["command"], json!("npx"));
    assert_eq!(example["args"], json!(["-y", "some-server"]));
}

#[test]
fn a_node_package_with_its_own_arguments_does_not_get_the_yes_flag() {
    // A package declaring arguments may well be passing its own flags first.
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "npm",
            "identifier": "some-server",
            "runtimeArguments": [{ "value": "--stdio" }],
        }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["args"], json!(["--stdio", "some-server"]));
}

#[test]
fn a_declared_runtime_hint_wins_over_the_default_launcher() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "pypi",
            "identifier": "some-server",
            "runtimeHint": "pipx",
        }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["command"], json!("pipx"));
}

#[test]
fn an_unrecognised_package_kind_is_launched_as_a_node_one() {
    // Most of the ecosystem is Node, so it is the least surprising guess when
    // the registry does not say.
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{ "registryType": "brew", "identifier": "some-server" }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["command"], json!("npx"));
}

// ---------------------------------------------------------------------------
// Cursors and page counts
// ---------------------------------------------------------------------------

#[test]
fn a_response_with_more_results_reports_one_page_beyond_the_current_one() {
    assert_eq!(page_bound(1, true), 2);
    assert_eq!(page_bound(7, true), 8);
}

#[test]
fn a_response_ending_the_results_reports_the_current_page() {
    assert_eq!(page_bound(1, false), 1);
    assert_eq!(page_bound(7, false), 7);
}

#[test]
fn the_page_bound_does_not_overflow() {
    assert_eq!(page_bound(u32::MAX, true), u32::MAX);
}

#[test]
fn a_cursor_is_read_from_the_metadata() {
    let response = list_response(&json!([]), Some("token-1"));
    assert_eq!(response.next_cursor(), Some("token-1"));
}

#[test]
fn an_empty_cursor_counts_as_no_cursor() {
    // The registry sends one at the end of a result set, and treating it as a
    // cursor would page forever.
    let response = list_response(&json!([]), Some(""));
    assert_eq!(response.next_cursor(), None);
}

#[test]
fn a_response_with_no_metadata_has_no_cursor() {
    assert_eq!(list_response(&json!([]), None).next_cursor(), None);
}

#[test]
fn a_cache_key_separates_query_page_and_size() {
    assert_eq!(
        search_cache_key("weather", 2, 50),
        "mcp_official:search:weather:2:50"
    );
    assert_ne!(search_cache_key("a", 1, 20), search_cache_key("a", 2, 20));
    assert_ne!(search_cache_key("a", 1, 20), search_cache_key("a", 1, 50));
    assert_ne!(search_cache_key("a", 1, 20), search_cache_key("b", 1, 20));
}

// ---------------------------------------------------------------------------
// The adapter against a loopback registry
// ---------------------------------------------------------------------------
//
// Everything above is shape conversion, which needs no server. What follows
// exercises the parts that only exist because the registry pages by opaque
// cursor: the walk, the two caches, and what happens when the chain ends early.
// Those are the paths a fixture cannot reach.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use axum::routing::get;
use parking_lot::Mutex;

use super::{MAX_CURSOR_WALK_PAGES, McpOfficialRegistry};
use crate::error::Error;
use crate::registry::Store;
use tinymcp_bus::McpRegistryAuthConfig;

/// What the mock registry saw.
#[derive(Debug, Default)]
struct Seen {
    pages: AtomicUsize,
    cursors: Mutex<Vec<Option<String>>>,
    authorization: Mutex<Option<String>>,
}

/// One query parameter off a request URI.
///
/// Read by hand: the workspace takes `axum` without default features, and the
/// query extractor is not among the few this suite needs enabled.
fn param(uri: &Uri, name: &str) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.replace('+', " "))
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

/// A registry whose result set runs to `pages` pages of one row each.
///
/// Cursors are the string form of the next page number, which is opaque enough
/// for the adapter — it never interprets one — and legible in a failure.
async fn paged_registry(pages: usize) -> (String, Arc<Seen>) {
    let seen = Arc::new(Seen::default());

    let app = Router::new()
        .route(
            "/v0/servers",
            get(
                move |State(seen): State<Arc<Seen>>, headers: HeaderMap, uri: Uri| async move {
                    seen.pages.fetch_add(1, Ordering::SeqCst);
                    *seen.authorization.lock() = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .map(ToString::to_string);

                    let cursor = param(&uri, "cursor");
                    seen.cursors.lock().push(cursor.clone());

                    let page: usize = cursor
                        .as_deref()
                        .and_then(|cursor| cursor.parse().ok())
                        .unwrap_or(1);

                    let mut body = json!({
                        "servers": [envelope(&format!("@acme/server-{page}"))],
                    });
                    if page < pages {
                        body["metadata"] = json!({ "nextCursor": (page + 1).to_string() });
                    }

                    axum::Json(body)
                },
            ),
        )
        .route(
            "/v0/servers/{*rest}",
            get(|State(seen): State<Arc<Seen>>, uri: Uri| async move {
                seen.pages.fetch_add(1, Ordering::SeqCst);
                // The path carries `{name}/versions`; the name is everything
                // before the trailing segment.
                let path = uri.path().trim_start_matches("/v0/servers/");
                let name = path.trim_end_matches("/versions");
                axum::Json(json!({ "servers": [envelope(name)] }))
            }),
        )
        .with_state(Arc::clone(&seen));

    (serve(app).await, seen)
}

/// A registry that answers every request with `status` and `body`.
async fn failing_registry(status: u16, body: &'static str) -> String {
    let app = Router::new().fallback(get(move || async move {
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            body.to_string(),
        )
    }));

    serve(app).await
}

/// Credentials naming `base` as the registry.
fn auth_at(base: &str) -> McpRegistryAuthConfig {
    McpRegistryAuthConfig {
        mcp_official_base: Some(base.to_string()),
        ..McpRegistryAuthConfig::default()
    }
}

/// An empty cursor map.
fn cursors() -> Mutex<HashMap<(String, u32, u32), String>> {
    Mutex::new(HashMap::new())
}

/// An empty store to cache into.
fn store() -> Store {
    Store::open_in_memory().expect("the store opens")
}

/// The adapter under test.
fn adapter() -> McpOfficialRegistry {
    McpOfficialRegistry::new().expect("the adapter builds")
}

// ---------------------------------------------------------------------------
// Searching
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_first_page_is_fetched_without_a_cursor() {
    let (base, seen) = paged_registry(3).await;

    let (servers, _) = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .expect("the search succeeds");

    assert_eq!(servers.len(), 1);
    assert_eq!(seen.cursors.lock().as_slice(), &[None]);
}

#[tokio::test]
async fn a_page_with_more_behind_it_reports_one_page_beyond() {
    // A bound, not a total: knowing the true count would mean walking the whole
    // chain, which is the cost this design exists to avoid. One page beyond is
    // what a caller needs to decide whether to offer a "next" control.
    let (base, _seen) = paged_registry(3).await;

    let (_, total_pages) = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(total_pages, 2);
}

#[tokio::test]
async fn the_last_page_reports_itself_as_the_last() {
    let (base, _seen) = paged_registry(1).await;

    let (_, total_pages) = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(total_pages, 1);
}

#[tokio::test]
async fn a_configured_token_is_sent_as_a_bearer() {
    let (base, seen) = paged_registry(1).await;
    let auth = McpRegistryAuthConfig {
        mcp_official_token: Some("tok-test".into()),
        ..auth_at(&base)
    };

    adapter()
        .search(&store(), &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(seen.authorization.lock().as_deref(), Some("Bearer tok-test"));
}

#[tokio::test]
async fn no_token_means_no_authorization_header() {
    let (base, seen) = paged_registry(1).await;

    adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(*seen.authorization.lock(), None);
}

// ---------------------------------------------------------------------------
// Paging over cursors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_warm_map_reaches_the_next_page_in_one_request() {
    // The whole point of keeping the map: paging sequentially must not re-walk
    // the chain each time.
    let (base, seen) = paged_registry(5).await;
    let auth = auth_at(&base);
    let cursors = cursors();
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors, "", 1, 20)
        .await
        .unwrap();
    let before = seen.pages.load(Ordering::SeqCst);

    adapter
        .search(&store, &auth, &cursors, "", 2, 20)
        .await
        .unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst) - before, 1);
}

#[tokio::test]
async fn a_cold_map_walks_forward_to_reach_a_deep_page() {
    // A link straight to page four, or the first search after a restart.
    let (base, seen) = paged_registry(5).await;

    let (servers, _) = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 4, 20)
        .await
        .expect("the walk reaches page four");

    assert_eq!(servers[0].qualified_name, "@acme/server-4");
    // Pages one through three to learn the cursors, then page four itself.
    assert_eq!(seen.pages.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_walk_fills_the_map_for_the_pages_it_passed() {
    // Otherwise walking to page four then asking for page three would walk
    // again, and paging backwards would cost more than paging forwards.
    let (base, seen) = paged_registry(5).await;
    let auth = auth_at(&base);
    let cursors = cursors();
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors, "", 4, 20)
        .await
        .unwrap();
    let before = seen.pages.load(Ordering::SeqCst);

    adapter
        .search(&store, &auth, &cursors, "", 3, 20)
        .await
        .unwrap();

    // Served from the stored page bodies the walk cached.
    assert_eq!(seen.pages.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn a_walk_reads_the_stored_cache_before_the_network() {
    // A cold in-memory map after a restart must not mean a cold network: the
    // page bodies from the previous run are still on disk.
    let (base, seen) = paged_registry(5).await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 4, 20)
        .await
        .unwrap();
    let before = seen.pages.load(Ordering::SeqCst);

    // A fresh map, as after a restart, against the same store.
    adapter
        .search(&store, &auth, &cursors(), "", 4, 20)
        .await
        .unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn a_page_past_the_end_of_the_chain_comes_back_empty() {
    // Rather than an error: the chain simply ran out, and an empty result
    // naming this page as the last is what stops a caller paging further.
    let (base, _seen) = paged_registry(2).await;

    let (servers, total_pages) = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 5, 20)
        .await
        .expect("running out of pages is not a failure");

    assert!(servers.is_empty());
    assert_eq!(total_pages, 5);
}

#[tokio::test]
async fn a_page_beyond_the_walk_limit_is_refused() {
    // A single request would otherwise fan into hundreds upstream, which is a
    // denial of service aimed at someone else.
    let (base, seen) = paged_registry(200).await;
    let target = MAX_CURSOR_WALK_PAGES + 1;

    let error = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", target, 20)
        .await
        .expect_err("beyond the walk limit");

    assert!(matches!(error, Error::MalformedResponse { .. }), "{error:?}");
    assert!(error.to_string().contains("page sequentially"), "{error}");
    assert_eq!(seen.pages.load(Ordering::SeqCst), 0, "nothing was requested");
}

// ---------------------------------------------------------------------------
// The response cache
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_repeated_search_is_served_from_the_stored_cache() {
    let (base, seen) = paged_registry(3).await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    let (servers, total_pages) = adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst), 1);
    assert_eq!(servers.len(), 1);
    assert_eq!(total_pages, 2);
}

#[tokio::test]
async fn a_cache_hit_still_records_the_cursor_it_carried() {
    // Otherwise a hit on page one would leave the map cold, and page two would
    // walk from the start — the cache would make paging slower.
    let (base, seen) = paged_registry(5).await;
    let auth = auth_at(&base);
    let store = store();
    let warm = cursors();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    // A fresh map reading the warm store, then straight on to page two.
    adapter
        .search(&store, &auth, &warm, "", 1, 20)
        .await
        .unwrap();
    let before = seen.pages.load(Ordering::SeqCst);

    adapter
        .search(&store, &auth, &warm, "", 2, 20)
        .await
        .unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst) - before, 1);
}

// ---------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_detail_lookup_takes_the_newest_version() {
    // The registry has no single-server endpoint, so a lookup reads the version
    // list, which leads with the newest.
    let (base, _seen) = paged_registry(1).await;

    let detail = adapter()
        .get(&store(), &auth_at(&base), "@acme/weather")
        .await
        .expect("the lookup succeeds");

    assert_eq!(detail.qualified_name, "@acme/weather");
}

#[tokio::test]
async fn a_repeated_detail_lookup_is_served_from_the_cache() {
    let (base, seen) = paged_registry(1).await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter.get(&store, &auth, "@acme/weather").await.unwrap();
    let detail = adapter.get(&store, &auth, "@acme/weather").await.unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst), 1);
    assert_eq!(detail.qualified_name, "@acme/weather");
}

#[tokio::test]
async fn a_registry_listing_no_versions_reports_an_unknown_server() {
    let app = Router::new().fallback(get(|| async { axum::Json(json!({ "servers": [] })) }));
    let base = serve(app).await;

    let error = adapter()
        .get(&store(), &auth_at(&base), "@acme/nothing")
        .await
        .expect_err("no versions");

    match error {
        Error::UnknownServer { server } => assert_eq!(server, "@acme/nothing"),
        other => panic!("expected an unknown-server error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_versions_body_that_is_not_json_is_reported_as_malformed() {
    let base = failing_registry(200, "not json").await;

    let error = adapter()
        .get(&store(), &auth_at(&base), "@acme/weather")
        .await
        .expect_err("unparseable");

    assert!(matches!(error, Error::MalformedResponse { .. }), "{error:?}");
}

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_upstream_failure_status_is_reported_with_its_body() {
    let base = failing_registry(503, "maintenance").await;

    let error = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .expect_err("503");

    match error {
        Error::Http { status, body, .. } => {
            assert_eq!(status, 503);
            assert_eq!(body, "maintenance");
        }
        other => panic!("expected an http error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_long_failure_body_is_truncated() {
    let base = failing_registry(500, concat!(
        "0123456789012345678901234567890123456789012345678901234567890123456789",
        "0123456789012345678901234567890123456789012345678901234567890123456789",
        "0123456789012345678901234567890123456789012345678901234567890123456789",
        "0123456789012345678901234567890123456789",
    ))
    .await;

    let error = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .expect_err("500");

    match error {
        Error::Http { body, .. } => assert_eq!(body.len(), 200),
        other => panic!("expected an http error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unreachable_registry_is_reported_as_a_transport_failure() {
    let error = adapter()
        .search(&store(), &auth_at("http://127.0.0.1:1"), &cursors(), "", 1, 20)
        .await
        .expect_err("unreachable");

    assert!(matches!(error, Error::Transport { .. }), "{error:?}");
}

#[tokio::test]
async fn a_list_body_that_does_not_decode_is_reported_as_malformed() {
    let base = failing_registry(200, "[1, 2, 3]").await;

    let error = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .expect_err("unparseable");

    assert!(matches!(error, Error::MalformedResponse { .. }), "{error:?}");
}
