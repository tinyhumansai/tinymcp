//! Unit tests for the startup connect pass.
//!
//! The property that matters most is that nothing here stops a host starting.
//! Each test that could plausibly abort the pass — a store that will not list,
//! a server that refuses, a mix of working and broken — asserts that the pass
//! completes and reports what happened.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::types::{BOOT_CONCURRENCY, BootOutcome, connect_installed_servers};
use crate::registry::{Connections, OAuthFlow, Store};
use tinymcp_bus::{CommandKind, InstalledServer, McpClientIdentityConfig, Transport};

/// An install with the given identifier and transport.
fn install(server_id: &str, transport: Transport, enabled: bool) -> InstalledServer {
    InstalledServer {
        server_id: server_id.to_string(),
        qualified_name: format!("@test/{server_id}"),
        display_name: server_id.to_string(),
        description: None,
        icon_url: None,
        command_kind: CommandKind::Node,
        command: "npx".into(),
        args: Vec::new(),
        env_keys: Vec::new(),
        config: None,
        installed_at: 1_000,
        last_connected_at: None,
        transport,
        enabled,
    }
}

/// Binds a loopback port and serves a working MCP server.
async fn serve_working_server() -> String {
    let app = Router::new().route(
        "/",
        post(|Json(body): Json<Value>| async move {
            let method = body
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let result = if method == "initialize" {
                json!({
                    "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "serverInfo": { "name": "working", "version": "1" },
                })
            } else {
                json!({ "tools": [{ "name": "forecast" }] })
            };
            Json(json!({ "jsonrpc": "2.0", "id": body["id"].clone(), "result": result }))
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/")
}

/// An install pointed at a port that refuses immediately.
fn unreachable(server_id: &str) -> InstalledServer {
    install(
        server_id,
        Transport::HttpRemote {
            url: "http://127.0.0.1:1/mcp".into(),
        },
        true,
    )
}

/// Runs a startup pass over `store`.
async fn boot(store: &Store, connections: &Connections) -> BootOutcome {
    let oauth = OAuthFlow::new(None).unwrap();
    connect_installed_servers(
        store,
        connections,
        &oauth,
        &McpClientIdentityConfig::default(),
        None,
    )
    .await
}

// ---------------------------------------------------------------------------
// The empty cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_host_with_no_installs_boots_to_nothing() {
    let store = Store::open_in_memory().unwrap();
    let connections = Connections::new();

    let outcome = boot(&store, &connections).await;

    assert_eq!(outcome, BootOutcome::default());
    assert_eq!(outcome.total(), 0);
    assert_eq!(connections.connected_count().await, 0);
}

// ---------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_enabled_install_is_connected() {
    let url = serve_working_server().await;
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&install("srv-1", Transport::HttpRemote { url }, true))
        .unwrap();
    let connections = Connections::new();

    let outcome = boot(&store, &connections).await;

    assert_eq!(outcome.connected, 1);
    assert_eq!(outcome.failed, 0);
    assert_eq!(outcome.skipped, 0);
    assert!(connections.is_connected("srv-1").await);
}

#[tokio::test]
async fn several_installs_are_all_connected() {
    let store = Store::open_in_memory().unwrap();
    for index in 0..5 {
        let url = serve_working_server().await;
        store
            .insert_server(&install(
                &format!("srv-{index}"),
                Transport::HttpRemote { url },
                true,
            ))
            .unwrap();
    }
    let connections = Connections::new();

    let outcome = boot(&store, &connections).await;

    assert_eq!(outcome.connected, 5);
    assert_eq!(connections.connected_count().await, 5);
}

#[tokio::test]
async fn more_installs_than_the_concurrency_limit_all_still_connect() {
    // The bound caps how many run at once, not how many run.
    let store = Store::open_in_memory().unwrap();
    let count = BOOT_CONCURRENCY + 3;
    for index in 0..count {
        let url = serve_working_server().await;
        store
            .insert_server(&install(
                &format!("srv-{index}"),
                Transport::HttpRemote { url },
                true,
            ))
            .unwrap();
    }
    let connections = Connections::new();

    let outcome = boot(&store, &connections).await;

    assert_eq!(outcome.connected, count);
    assert_eq!(connections.connected_count().await, count);
}

// ---------------------------------------------------------------------------
// Disabled installs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_disabled_install_is_skipped_rather_than_attempted() {
    let url = serve_working_server().await;
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&install("srv-off", Transport::HttpRemote { url }, false))
        .unwrap();
    let connections = Connections::new();

    let outcome = boot(&store, &connections).await;

    assert_eq!(outcome.skipped, 1);
    assert_eq!(outcome.connected, 0);
    assert!(!connections.is_connected("srv-off").await);
    // Skipping is not failing: nothing was tried, so nothing was recorded.
    assert_eq!(connections.last_error("srv-off").await, None);
}

// ---------------------------------------------------------------------------
// Failures do not stop the pass
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_server_that_cannot_connect_does_not_stop_startup() {
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&unreachable("srv-broken")).unwrap();
    let connections = Connections::new();

    let outcome = boot(&store, &connections).await;

    assert_eq!(outcome.failed, 1);
    assert_eq!(outcome.connected, 0);
}

#[tokio::test]
async fn one_broken_server_does_not_cost_the_others() {
    // The case this exists for: MCP servers are third-party subprocesses and
    // third-party endpoints, so one of them being broken is the expected case.
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&unreachable("srv-broken")).unwrap();
    for index in 0..3 {
        let url = serve_working_server().await;
        store
            .insert_server(&install(
                &format!("srv-good-{index}"),
                Transport::HttpRemote { url },
                true,
            ))
            .unwrap();
    }
    let connections = Connections::new();

    let outcome = boot(&store, &connections).await;

    assert_eq!(outcome.connected, 3);
    assert_eq!(outcome.failed, 1);
    assert_eq!(connections.connected_count().await, 3);
}

#[tokio::test]
async fn a_failure_is_recorded_so_the_supervisor_and_a_status_read_can_see_it() {
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&unreachable("srv-broken")).unwrap();
    let connections = Connections::new();

    boot(&store, &connections).await;

    assert!(connections.last_error("srv-broken").await.is_some());
}

// ---------------------------------------------------------------------------
// The outcome
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_outcome_accounts_for_every_install() {
    let url = serve_working_server().await;
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&install("srv-good", Transport::HttpRemote { url }, true))
        .unwrap();
    store.insert_server(&unreachable("srv-broken")).unwrap();
    store
        .insert_server(&install("srv-off", Transport::Stdio, false))
        .unwrap();
    let connections = Connections::new();

    let outcome = boot(&store, &connections).await;

    assert_eq!(outcome.connected, 1);
    assert_eq!(outcome.failed, 1);
    assert_eq!(outcome.skipped, 1);
    assert_eq!(outcome.total(), 3);
}

#[test]
fn an_empty_outcome_totals_nothing() {
    assert_eq!(BootOutcome::default().total(), 0);
}
