//! Unit tests for the reconnect supervisor.
//!
//! The backoff curve is tested directly against an injected clock, because its
//! whole job is to be right about time and a test that waited for real time
//! would be slow and flaky in equal measure. The cycle is driven one tick at a
//! time for the same reason.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::backoff::{BACKOFF_BASE, BACKOFF_MAX, BackoffState, delay_after};
use super::types::{Supervisor, SupervisorConfig};
use crate::registry::{Connections, OAuthFlow, Store};
use tinymcp_bus::{CommandKind, InstalledServer, McpClientIdentityConfig, Transport};

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

#[test]
fn the_delay_doubles_with_each_consecutive_failure() {
    assert_eq!(delay_after(0), BACKOFF_BASE);
    assert_eq!(delay_after(1), Duration::from_secs(5));
    assert_eq!(delay_after(2), Duration::from_secs(10));
    assert_eq!(delay_after(3), Duration::from_secs(20));
    assert_eq!(delay_after(4), Duration::from_secs(40));
}

#[test]
fn the_delay_is_capped_so_a_long_down_server_is_still_retried() {
    // Its operator may fix it without anyone touching this host, and an
    // uncapped curve would leave it unreachable for hours afterwards.
    assert_eq!(delay_after(20), BACKOFF_MAX);
}

#[test]
fn an_absurd_failure_count_does_not_overflow_into_a_short_delay() {
    // A server failing for weeks must produce the longest delay, not a wrapped
    // one that hammers it every few seconds.
    assert_eq!(delay_after(u32::MAX), BACKOFF_MAX);
    assert_eq!(delay_after(64), BACKOFF_MAX);
    assert_eq!(delay_after(65), BACKOFF_MAX);
}

#[test]
fn a_state_with_no_failures_is_ready_at_once() {
    assert!(BackoffState::default().ready(Instant::now()));
}

#[test]
fn a_failure_defers_the_next_attempt_until_the_delay_has_passed() {
    let base = Instant::now();
    let mut state = BackoffState::default();

    state.record_failure(base);

    assert_eq!(state.failures, 1);
    assert!(!state.ready(base));
    assert!(!state.ready(base + Duration::from_secs(4)));
    assert!(state.ready(base + Duration::from_secs(5)));
}

#[test]
fn consecutive_failures_lengthen_the_window() {
    let base = Instant::now();
    let mut state = BackoffState::default();

    state.record_failure(base);
    state.record_failure(base);

    assert_eq!(state.failures, 2);
    assert!(!state.ready(base + Duration::from_secs(9)));
    assert!(state.ready(base + Duration::from_secs(10)));
}

#[test]
fn a_state_reports_the_delay_its_failure_count_implies() {
    let mut state = BackoffState::default();
    assert_eq!(state.current_delay(), BACKOFF_BASE);

    state.record_failure(Instant::now());
    state.record_failure(Instant::now());
    assert_eq!(state.current_delay(), Duration::from_secs(10));
}

// ---------------------------------------------------------------------------
// The cycle
// ---------------------------------------------------------------------------

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

/// A supervisor with a short probe timeout, for tests.
fn supervisor() -> Supervisor {
    Supervisor::new(
        SupervisorConfig {
            tick_interval: Duration::from_millis(10),
            probe_timeout: Duration::from_secs(5),
        },
        McpClientIdentityConfig::default(),
        None,
    )
}

#[tokio::test]
async fn a_tick_connects_a_server_that_is_not_connected() {
    let url = serve_working_server().await;
    let server = install("srv-1", Transport::HttpRemote { url }, true);
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&server).unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    supervisor()
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert!(connections.is_connected("srv-1").await);
}

#[tokio::test]
async fn a_tick_leaves_a_healthy_connection_alone() {
    let url = serve_working_server().await;
    let server = install("srv-1", Transport::HttpRemote { url }, true);
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&server).unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();

    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;
    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert!(connections.is_connected("srv-1").await);
    assert_eq!(supervisor.backed_off_count(), 0);
}

#[tokio::test]
async fn a_disabled_server_is_not_connected() {
    let url = serve_working_server().await;
    let server = install("srv-off", Transport::HttpRemote { url }, false);
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&server).unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    supervisor()
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert!(!connections.is_connected("srv-off").await);
}

#[tokio::test]
async fn a_disabled_server_carries_no_backoff_penalty_into_being_re_enabled() {
    // Otherwise turning a server back on would sit behind a delay earned before
    // the user switched it off.
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&install(
            "srv-1",
            Transport::HttpRemote {
                url: "http://127.0.0.1:1/mcp".into(),
            },
            true,
        ))
        .unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();

    // Fail once, earning a penalty.
    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;
    assert_eq!(supervisor.backed_off_count(), 1);

    // Now the user disables it.
    store.update_enabled("srv-1", false).unwrap();
    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert_eq!(supervisor.backed_off_count(), 0);
}

#[tokio::test]
async fn a_failed_reconnect_earns_a_backoff_penalty() {
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&install(
            "srv-1",
            Transport::HttpRemote {
                url: "http://127.0.0.1:1/mcp".into(),
            },
            true,
        ))
        .unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();

    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert!(!connections.is_connected("srv-1").await);
    assert_eq!(supervisor.backed_off_count(), 1);
    assert!(connections.last_error("srv-1").await.is_some());
}

#[tokio::test]
async fn a_server_inside_its_backoff_window_is_not_retried() {
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&install(
            "srv-1",
            Transport::HttpRemote {
                url: "http://127.0.0.1:1/mcp".into(),
            },
            true,
        ))
        .unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();
    let base = Instant::now();

    supervisor.tick(&store, &connections, &oauth, base).await;
    // A second tick one second later is inside the five-second window, so the
    // failure count must not grow.
    supervisor
        .tick(&store, &connections, &oauth, base + Duration::from_secs(1))
        .await;

    assert_eq!(supervisor.backed_off_count(), 1);
}

#[tokio::test]
async fn a_server_whose_window_has_passed_is_retried() {
    let url = serve_working_server().await;
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&install(
            "srv-1",
            Transport::HttpRemote {
                url: "http://127.0.0.1:1/mcp".into(),
            },
            true,
        ))
        .unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();
    let base = Instant::now();

    supervisor.tick(&store, &connections, &oauth, base).await;
    assert_eq!(supervisor.backed_off_count(), 1);

    // The server comes back, and the window has elapsed.
    store.delete_server("srv-1").unwrap();
    store
        .insert_server(&install("srv-1", Transport::HttpRemote { url }, true))
        .unwrap();
    supervisor
        .tick(&store, &connections, &oauth, base + Duration::from_secs(30))
        .await;

    assert!(connections.is_connected("srv-1").await);
    assert_eq!(supervisor.backed_off_count(), 0);
}

#[tokio::test]
async fn a_tick_over_an_empty_store_does_nothing() {
    let store = Store::open_in_memory().unwrap();
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();

    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert_eq!(supervisor.backed_off_count(), 0);
    assert_eq!(connections.connected_count().await, 0);
}

#[test]
fn the_default_pacing_is_a_minute_with_an_eight_second_probe() {
    let config = SupervisorConfig::default();
    assert_eq!(config.tick_interval, Duration::from_secs(60));
    assert_eq!(config.probe_timeout, Duration::from_secs(8));
}
