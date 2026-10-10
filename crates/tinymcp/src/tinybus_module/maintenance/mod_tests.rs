//! Unit tests for the module's background work.
//!
//! Nothing here waits on a wall clock. The boot pass is awaited through
//! [`Maintenance::booted`], and the supervisor's cycles are either sent by the
//! test ([`Pacing::Driven`]) or, for the interval pacing, produced by a paused
//! clock over a store with no installs — so there is no network I/O for the
//! clock's auto-advance to race.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::Maintenance;
use super::types::Pacing;
use crate::registry::{McpRegistry, Store, SupervisorConfig, SupervisorEvent};
use tinymcp_bus::{
    CommandKind, InstalledServer, McpClientIdentityConfig, McpRegistryAuthConfig, Transport,
};

/// A registry over an in-memory store holding `servers`.
fn registry_with(servers: &[InstalledServer]) -> Arc<McpRegistry> {
    let store = Store::open_in_memory().unwrap();
    for server in servers {
        store.insert_server(server).unwrap();
    }
    Arc::new(
        McpRegistry::new(
            store,
            McpRegistryAuthConfig::default(),
            McpClientIdentityConfig::default(),
            None,
        )
        .unwrap(),
    )
}

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

/// A driven maintenance task over `registry`, with its cycle and report
/// channels.
fn driven(
    registry: &Arc<McpRegistry>,
) -> (
    Maintenance,
    mpsc::Sender<()>,
    mpsc::UnboundedReceiver<crate::registry::TickReport>,
) {
    let (cycles, cycle_rx) = mpsc::channel(4);
    let (reports_tx, reports) = mpsc::unbounded_channel();
    let maintenance = Maintenance::start_with(
        Arc::clone(registry),
        SupervisorConfig::default(),
        Pacing::Driven(cycle_rx),
        Some(reports_tx),
    );
    (maintenance, cycles, reports)
}

// ---------------------------------------------------------------------------
// The boot pass
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_installed_server_is_connected_without_anyone_asking() {
    let url = serve_working_server().await;
    let registry = registry_with(&[install("srv-1", Transport::HttpRemote { url }, true)]);

    let (maintenance, _cycles, _reports) = driven(&registry);
    let outcome = maintenance.booted().await;

    assert_eq!(outcome.connected, 1);
    assert!(registry.connections().is_connected("srv-1").await);
}

#[tokio::test]
async fn a_server_the_user_turned_off_is_left_alone_at_boot() {
    let url = serve_working_server().await;
    let registry = registry_with(&[install("srv-off", Transport::HttpRemote { url }, false)]);

    let (maintenance, _cycles, _reports) = driven(&registry);
    let outcome = maintenance.booted().await;

    assert_eq!(outcome.skipped, 1);
    assert!(!registry.connections().is_connected("srv-off").await);
}

#[tokio::test]
async fn a_server_that_cannot_connect_is_reported_and_does_not_end_the_task() {
    let registry = registry_with(&[install(
        "srv-down",
        Transport::HttpRemote {
            url: "http://127.0.0.1:1/mcp".into(),
        },
        true,
    )]);

    let (maintenance, cycles, mut reports) = driven(&registry);
    let outcome = maintenance.booted().await;
    assert_eq!(outcome.failed, 1);

    // The supervisor is still running after a failed boot: a cycle is answered.
    cycles.send(()).await.unwrap();
    assert!(reports.recv().await.is_some());
}

#[tokio::test]
async fn a_registry_with_nothing_installed_boots_to_nothing() {
    let registry = registry_with(&[]);

    let (maintenance, _cycles, _reports) = driven(&registry);

    assert_eq!(maintenance.booted().await.total(), 0);
}

// ---------------------------------------------------------------------------
// The supervisor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_dropped_connection_is_restored_by_the_next_cycle() {
    let url = serve_working_server().await;
    let registry = registry_with(&[install("srv-1", Transport::HttpRemote { url }, true)]);
    let (maintenance, cycles, mut reports) = driven(&registry);
    maintenance.booted().await;
    assert!(registry.connections().is_connected("srv-1").await);

    // Drop the connection out from under the module, the way a session expiry
    // would, then let one cycle run.
    registry.connections().disconnect("srv-1").await;
    cycles.send(()).await.unwrap();
    let report = reports.recv().await.unwrap();

    assert!(
        report
            .events
            .iter()
            .any(|event| matches!(event, SupervisorEvent::Reconnected { .. })),
        "{report:?}"
    );
    assert!(registry.connections().is_connected("srv-1").await);
    let batch = maintenance.drain(256).unwrap();
    assert_eq!(batch.events, report.events);
    assert_eq!(batch.dropped, 0);
    assert!(maintenance.drain(256).unwrap().events.is_empty());
}

#[tokio::test]
async fn a_healthy_connection_is_probed_and_kept() {
    let url = serve_working_server().await;
    let registry = registry_with(&[install("srv-1", Transport::HttpRemote { url }, true)]);
    let (maintenance, cycles, mut reports) = driven(&registry);
    maintenance.booted().await;

    cycles.send(()).await.unwrap();
    let report = reports.recv().await.unwrap();

    assert!(
        report
            .events
            .iter()
            .any(|event| matches!(event, SupervisorEvent::ProbeAnswered { .. })),
        "{report:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_interval_pacing_runs_one_cycle_per_interval_and_not_before() {
    let registry = registry_with(&[]);
    let interval = Duration::from_secs(60);
    let (reports_tx, mut reports) = mpsc::unbounded_channel();
    let maintenance = Maintenance::start_with(
        Arc::clone(&registry),
        SupervisorConfig {
            tick_interval: interval,
            ..SupervisorConfig::default()
        },
        Pacing::Interval(interval),
        Some(reports_tx),
    );
    maintenance.booted().await;

    // Just short of the first interval: nothing has run.
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert!(reports.try_recv().is_err());

    // Reaching it runs exactly one cycle.
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(reports.recv().await.is_some());
    assert!(reports.try_recv().is_err());

    // And the next interval runs the next one.
    tokio::time::advance(interval).await;
    assert!(reports.recv().await.is_some());
}

#[tokio::test(start_paused = true)]
async fn start_paces_by_the_supervisor_configuration() {
    // `start` is the production entry; it must not need a driver.
    let registry = registry_with(&[]);
    let maintenance = Maintenance::start(Arc::clone(&registry), SupervisorConfig::default());

    assert_eq!(maintenance.booted().await.total(), 0);
}

// ---------------------------------------------------------------------------
// Lifetime
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dropping_the_handle_ends_the_task() {
    let registry = registry_with(&[]);
    let (maintenance, cycles, _reports) = driven(&registry);
    maintenance.booted().await;

    drop(maintenance);

    // The task owned the receiving half; once it is gone the channel closes.
    cycles.closed().await;
    assert!(cycles.send(()).await.is_err());
}

#[tokio::test]
async fn a_closed_cycle_channel_ends_the_supervisor() {
    let registry = registry_with(&[]);
    let (maintenance, cycles, _reports) = driven(&registry);
    maintenance.booted().await;

    drop(cycles);

    // The boot signal stays readable after the task has finished.
    assert_eq!(maintenance.booted().await.total(), 0);
}
