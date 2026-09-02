//! Unit tests for the reconnect supervisor.
//!
//! The backoff curve is tested directly against an injected clock, because its
//! whole job is to be right about time and a test that waited for real time
//! would be slow and flaky in equal measure. The cycle is driven one tick at a
//! time for the same reason.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::backoff::{BACKOFF_BASE, BACKOFF_MAX, BackoffState, delay_after};
use super::report::{ServerRef, SupervisorEvent, TickReport};
use super::types::{Supervisor, SupervisorConfig};
use crate::registry::{Connections, OAuthFlow, ProbeOutcome, Store};
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
async fn a_missing_runtime_is_terminal_and_earns_no_backoff_penalty() {
    // A binary that is not installed will not appear because we waited five
    // minutes, so the supervisor must park the server instead of scheduling
    // another attempt against it.
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&install("srv-1", Transport::Stdio, true))
        .unwrap();
    // `install` launches through `npx`; an empty PATH forces the
    // missing-command branch whether or not this machine has Node.
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([(
                "PATH".to_string(),
                "/tinymcp/deliberately/does/not/exist".to_string(),
            )]),
        )
        .unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();
    let base = Instant::now();

    supervisor.tick(&store, &connections, &oauth, base).await;

    assert!(!connections.is_connected("srv-1").await);
    assert_eq!(
        supervisor.backed_off_count(),
        0,
        "a backoff promises that waiting helps, and here it cannot"
    );
    assert_eq!(supervisor.terminally_failed_count(), 1);

    // Far past any backoff window, so a penalised server would certainly be
    // retried by now. A parked one must not be.
    let first_error = connections.last_error("srv-1").await;
    assert!(first_error.is_some(), "the first attempt is still reported");
    supervisor
        .tick(&store, &connections, &oauth, base + BACKOFF_MAX * 2)
        .await;

    assert_eq!(supervisor.backed_off_count(), 0);
    assert_eq!(supervisor.terminally_failed_count(), 1);

    // Disabling clears the verdict, so installing the runtime and toggling the
    // server is a way back.
    store.update_enabled("srv-1", false).unwrap();
    supervisor
        .tick(&store, &connections, &oauth, base + BACKOFF_MAX * 3)
        .await;

    assert_eq!(supervisor.terminally_failed_count(), 0);
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
fn the_default_probe_window_is_shorter_than_a_real_request_budget() {
    let config = SupervisorConfig::default();
    assert_eq!(config.tick_interval, Duration::from_secs(60));
    assert_eq!(config.probe_timeout, Duration::from_secs(8));
    // The probe is an early signal, not a verdict, so its window is
    // deliberately tighter than the budget a real call gets. What keeps a
    // merely-slow server from being torn down is the consecutive-timeout run,
    // not an equal deadline — and the window also bounds the worst-case cycle,
    // since `tick` probes installs in sequence.
    assert!(
        config.probe_timeout < crate::REMOTE_REQUEST_TIMEOUT,
        "the probe window must stay tighter than the request budget"
    );
}

// ---------------------------------------------------------------------------
// The loop itself
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_first_tick_waits_a_whole_interval_before_it_runs() {
    // So it does not race the startup connect pass: reconnecting a server that
    // is halfway through connecting would tear down work already in flight.
    let store = Store::open_in_memory().unwrap();
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    let supervisor = Supervisor::new(
        SupervisorConfig {
            tick_interval: Duration::from_secs(30),
            ..SupervisorConfig::default()
        },
        McpClientIdentityConfig::default(),
        None,
    );

    // The loop never returns, so it is raced against a clock this test owns.
    // Reaching the timeout is the assertion: the first tick has not fired.
    let running = tokio::spawn(async move {
        let store = Store::open_in_memory().unwrap();
        let connections = Connections::new();
        let oauth = OAuthFlow::new(None).unwrap();
        supervisor.run(&store, &connections, &oauth).await;
    });

    tokio::time::sleep(Duration::from_secs(90)).await;

    assert!(!running.is_finished(), "the supervisor loop ended");
    running.abort();
    drop((store, connections, oauth));
}

#[tokio::test]
async fn a_tick_over_an_empty_store_does_nothing_and_does_not_fail() {
    let mut supervisor = Supervisor::new(
        SupervisorConfig::default(),
        McpClientIdentityConfig::default(),
        None,
    );

    supervisor
        .tick(
            &Store::open_in_memory().unwrap(),
            &Connections::new(),
            &OAuthFlow::new(None).unwrap(),
            Instant::now(),
        )
        .await;
}

#[tokio::test]
async fn a_tick_leaves_a_disabled_install_alone() {
    // The disable path owns tearing the connection down. All the supervisor
    // does is forget the backoff, so re-enabling gets an immediate attempt
    // rather than inheriting an old penalty.
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&install("srv-1", Transport::Stdio, false))
        .unwrap();

    let connections = Connections::new();
    let mut supervisor = Supervisor::new(
        SupervisorConfig::default(),
        McpClientIdentityConfig::default(),
        None,
    );

    supervisor
        .tick(
            &store,
            &connections,
            &OAuthFlow::new(None).unwrap(),
            Instant::now(),
        )
        .await;

    assert_eq!(connections.connected_count().await, 0);
}

#[tokio::test]
async fn a_tick_that_cannot_read_the_store_gives_up_quietly_rather_than_failing_the_loop() {
    // The supervisor runs forever. A store read that fails once — a locked
    // file, a disk hiccup — must cost one cycle, not the process's ability to
    // ever reconnect anything.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store.db");
    let store = Store::open_file(&path).unwrap();
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute("DROP TABLE mcp_servers", []).unwrap();
    }

    let mut supervisor = Supervisor::new(
        SupervisorConfig::default(),
        McpClientIdentityConfig::default(),
        None,
    );

    // Returns rather than panicking or propagating.
    supervisor
        .tick(
            &store,
            &Connections::new(),
            &OAuthFlow::new(None).unwrap(),
            Instant::now(),
        )
        .await;
}

#[tokio::test]
async fn a_dropped_transport_is_noticed_and_reconnected() {
    // The reason the supervisor exists: an MCP transport can go away silently
    // while its entry sits in the map looking fine, and a user's next tool call
    // is the wrong place to find that out.
    let endpoint = serve_working_server().await;
    let server = install("srv-1", Transport::HttpRemote { url: endpoint }, true);
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&server).unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    connections
        .connect(
            &store,
            &oauth,
            &McpClientIdentityConfig::default(),
            None,
            &server,
        )
        .await
        .expect("the first connect");

    let mut supervisor = Supervisor::new(
        SupervisorConfig::default(),
        McpClientIdentityConfig::default(),
        None,
    );

    // A live server: the probe succeeds and the entry is left alone.
    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;
    assert_eq!(connections.connected_count().await, 1);
}

#[tokio::test]
async fn a_server_that_cannot_be_reconnected_earns_a_growing_backoff() {
    // Otherwise a permanently broken server is dialled on every cycle forever,
    // which is a retry storm aimed at someone else's endpoint.
    let server = install(
        "srv-1",
        Transport::HttpRemote {
            url: "http://127.0.0.1:1/mcp".into(),
        },
        true,
    );
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&server).unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = Supervisor::new(
        SupervisorConfig::default(),
        McpClientIdentityConfig::default(),
        None,
    );

    let now = Instant::now();
    supervisor.tick(&store, &connections, &oauth, now).await;
    assert_eq!(connections.connected_count().await, 0);

    // Immediately again: the backoff window has not passed, so nothing is
    // dialled and the failure is not compounded.
    supervisor.tick(&store, &connections, &oauth, now).await;
    assert_eq!(connections.connected_count().await, 0);
}

// ---------------------------------------------------------------------------
// Probe outcomes
//
// The supervisor used to collapse every failed probe into one bool and then
// report it as "the transport dropped". A server that is up but answers a
// `tools/list` more slowly than the probe window was therefore disconnected by
// the supervisor itself, and the reconnect that followed was repairing damage
// the supervisor had caused. These cover the three outcomes separately.
//
// Each test makes the teardown *observable* by refusing the reconnect: with
// `initialize` failing, a session that is torn down cannot come back, so the
// connected count distinguishes "left alone" from "dropped and rebuilt" —
// which a count alone cannot do while reconnects succeed.
// ---------------------------------------------------------------------------

/// How a [`serve_adjustable_server`] answers.
#[derive(Debug)]
struct ServerDials {
    /// How long `tools/list` takes before answering.
    list_delay: std::sync::atomic::AtomicU64,
    /// Whether `tools/list` answers with a JSON-RPC error instead of tools.
    list_errors: std::sync::atomic::AtomicBool,
    /// Whether the *next* `tools/list` alone answers with an error.
    ///
    /// One failure, then back to normal: the shape of a transport that
    /// hiccups once and is fine again by the time the reconnect lists tools.
    fail_next_list: std::sync::atomic::AtomicBool,
    /// Whether `initialize` succeeds, i.e. whether a reconnect can work.
    initialize_ok: std::sync::atomic::AtomicBool,
}

impl ServerDials {
    fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            list_delay: std::sync::atomic::AtomicU64::new(0),
            list_errors: std::sync::atomic::AtomicBool::new(false),
            fail_next_list: std::sync::atomic::AtomicBool::new(false),
            initialize_ok: std::sync::atomic::AtomicBool::new(true),
        })
    }

    /// Fail the next `tools/list` only, so a probe finds a broken transport
    /// but the reconnect that follows can still complete.
    fn fail_next_list(&self) {
        self.fail_next_list
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn set_list_delay(&self, delay: Duration) {
        self.list_delay.store(
            u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    fn set_list_errors(&self, errors: bool) {
        self.list_errors
            .store(errors, std::sync::atomic::Ordering::SeqCst);
    }

    /// Stop answering `initialize`, so a torn-down session cannot be rebuilt.
    fn refuse_reconnects(&self) {
        self.initialize_ok
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Binds a loopback port and serves an MCP server that can be made slow,
/// broken, or unreconnectable while the test runs.
///
/// `initialize` is separate from `tools/list` on purpose: a connect has to be
/// able to succeed before the probe behaviour under test matters.
async fn serve_adjustable_server(dials: &std::sync::Arc<ServerDials>) -> String {
    let dials = std::sync::Arc::clone(dials);
    let app = Router::new().route(
        "/",
        post(move |Json(body): Json<Value>| {
            let dials = std::sync::Arc::clone(&dials);
            async move {
                let method = body
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let id = body["id"].clone();

                if method == "initialize" {
                    if !dials
                        .initialize_ok
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        return Json(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32000, "message": "not accepting sessions" },
                        }));
                    }
                    return Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                            "capabilities": {},
                            "serverInfo": { "name": "adjustable", "version": "1" },
                        },
                    }));
                }

                let delay = dials.list_delay.load(std::sync::atomic::Ordering::SeqCst);
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }

                let fail_once = dials
                    .fail_next_list
                    .swap(false, std::sync::atomic::Ordering::SeqCst);
                if fail_once || dials.list_errors.load(std::sync::atomic::Ordering::SeqCst) {
                    return Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32001, "message": "the session is gone" },
                    }));
                }

                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "tools": [{ "name": "forecast" }] },
                }))
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/")
}

/// A probe window short enough to exceed on purpose.
const TEST_PROBE_TIMEOUT: Duration = Duration::from_millis(100);

/// Comfortably past [`TEST_PROBE_TIMEOUT`], and still quick to schedule.
const SLOWER_THAN_THE_PROBE: Duration = Duration::from_millis(1_500);

fn probing_supervisor() -> Supervisor {
    Supervisor::new(
        SupervisorConfig {
            tick_interval: Duration::from_millis(10),
            probe_timeout: TEST_PROBE_TIMEOUT,
        },
        McpClientIdentityConfig::default(),
        None,
    )
}

/// Connects `server` and hands back everything a tick needs.
async fn connected_to(url: String) -> (Store, Connections, OAuthFlow, InstalledServer) {
    let server = install("srv-1", Transport::HttpRemote { url }, true);
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&server).unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    connections
        .connect(
            &store,
            &oauth,
            &McpClientIdentityConfig::default(),
            None,
            &server,
        )
        .await
        .expect("the first connect");

    (store, connections, oauth, server)
}

#[tokio::test]
async fn one_slow_probe_leaves_a_working_session_alone() {
    // The regression this whole change is about. A single probe that runs out
    // of window is evidence of slowness, not of a drop, and acting on it makes
    // the supervisor the cause of the outage it goes on to report.
    let dials = ServerDials::new();
    let url = serve_adjustable_server(&dials).await;
    let (store, connections, oauth, _server) = connected_to(url).await;

    dials.set_list_delay(SLOWER_THAN_THE_PROBE);
    dials.refuse_reconnects();

    let mut supervisor = probing_supervisor();
    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert_eq!(
        connections.connected_count().await,
        1,
        "a single slow probe must not end the session"
    );
    assert_eq!(
        supervisor.consecutive_timeouts("srv-1"),
        1,
        "the timeout should be counted rather than acted on"
    );
    assert_eq!(
        supervisor.backed_off_count(),
        0,
        "nothing was reconnected, so nothing should carry a reconnect penalty"
    );
}

#[tokio::test]
async fn a_run_of_slow_probes_does_eventually_end_the_session() {
    // The other half of the trade-off: a server that never answers has to be
    // recovered, or "do not act on one timeout" becomes "never act at all".
    let dials = ServerDials::new();
    let url = serve_adjustable_server(&dials).await;
    let (store, connections, oauth, _server) = connected_to(url).await;

    dials.set_list_delay(SLOWER_THAN_THE_PROBE);
    dials.refuse_reconnects();

    let mut supervisor = probing_supervisor();

    for tick in 1..=2 {
        supervisor
            .tick(&store, &connections, &oauth, Instant::now())
            .await;
        assert_eq!(
            connections.connected_count().await,
            1,
            "the session should survive timeout {tick} of 3"
        );
    }

    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert_eq!(
        connections.connected_count().await,
        0,
        "the third consecutive timeout should end the session"
    );
    assert_eq!(
        supervisor.consecutive_timeouts("srv-1"),
        0,
        "the streak is spent once it has been acted on"
    );
    assert_eq!(
        supervisor.backed_off_count(),
        1,
        "the refused reconnect should earn a backoff penalty"
    );
}

#[tokio::test]
async fn an_answered_probe_clears_the_timeout_streak() {
    // Otherwise timeouts accumulate across hours of healthy operation and a
    // server is eventually torn down for three slow answers that were nowhere
    // near each other.
    let dials = ServerDials::new();
    let url = serve_adjustable_server(&dials).await;
    let (store, connections, oauth, _server) = connected_to(url).await;

    let mut supervisor = probing_supervisor();

    dials.set_list_delay(SLOWER_THAN_THE_PROBE);
    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;
    assert_eq!(supervisor.consecutive_timeouts("srv-1"), 1);

    dials.set_list_delay(Duration::ZERO);
    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;
    assert_eq!(
        supervisor.consecutive_timeouts("srv-1"),
        0,
        "one answer should reset the run"
    );
    assert_eq!(connections.connected_count().await, 1);
}

#[tokio::test]
async fn a_transport_that_answers_with_an_error_is_torn_down_at_once() {
    // No regression to the case the supervisor was built for: a transport that
    // was *observed* to fail has nothing left to wait for, so it is not put
    // behind the consecutive-timeout threshold.
    let dials = ServerDials::new();
    let url = serve_adjustable_server(&dials).await;
    let (store, connections, oauth, _server) = connected_to(url).await;

    dials.set_list_errors(true);
    dials.refuse_reconnects();

    let mut supervisor = probing_supervisor();
    supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert_eq!(
        connections.connected_count().await,
        0,
        "a broken transport should be dropped on the first sighting"
    );
    assert_eq!(
        supervisor.consecutive_timeouts("srv-1"),
        0,
        "a transport error is not a timeout and must not fill the streak"
    );
    assert_eq!(supervisor.backed_off_count(), 1);
}

// ---------------------------------------------------------------------------
// The report
//
// A tick hands back what it observed so a host can route it: a log line cannot
// be filtered into an event log or turned into a notification, and which
// observations a user should hear about is the host's call, not this crate's.
// These pin that every branch of the cycle reports itself, carrying the numbers
// a host needs to make that call — the streak, the failure count, whether a
// rebuild happened within the cycle or after the server had stayed down.
// ---------------------------------------------------------------------------

fn kinds(report: &TickReport) -> Vec<&'static str> {
    report.events.iter().map(SupervisorEvent::kind).collect()
}

#[tokio::test]
async fn an_answered_probe_is_reported() {
    let dials = ServerDials::new();
    let url = serve_adjustable_server(&dials).await;
    let (store, connections, oauth, server) = connected_to(url).await;
    let mut supervisor = probing_supervisor();

    let report = supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert!(!report.is_empty(), "a healthy server is an observation too");
    match report.events.as_slice() {
        [
            SupervisorEvent::ProbeAnswered {
                server: reported, ..
            },
        ] => assert_eq!(*reported, ServerRef::from(&server)),
        other => panic!("expected one probe_answered event, got {other:?}"),
    }
}

#[tokio::test]
async fn a_kept_timeout_is_reported_with_its_place_in_the_streak() {
    let dials = ServerDials::new();
    let url = serve_adjustable_server(&dials).await;
    let (store, connections, oauth, server) = connected_to(url).await;
    dials.set_list_delay(SLOWER_THAN_THE_PROBE);
    dials.refuse_reconnects();
    let mut supervisor = probing_supervisor();

    let report = supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert_eq!(
        report.events,
        [SupervisorEvent::ProbeTimedOut {
            server: ServerRef::from(&server),
            after: TEST_PROBE_TIMEOUT,
            consecutive: 1,
            teardown_after: 3,
        }]
    );
}

#[tokio::test]
async fn the_teardown_after_a_run_of_timeouts_reports_the_drop_and_the_refused_reconnect() {
    let dials = ServerDials::new();
    let url = serve_adjustable_server(&dials).await;
    let (store, connections, oauth, server) = connected_to(url).await;
    dials.set_list_delay(SLOWER_THAN_THE_PROBE);
    dials.refuse_reconnects();
    let mut supervisor = probing_supervisor();

    for _ in 0..2 {
        supervisor
            .tick(&store, &connections, &oauth, Instant::now())
            .await;
    }
    let report = supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert_eq!(kinds(&report), ["transport_dropped", "reconnect_failed"]);
    match report.events.as_slice() {
        [
            dropped,
            SupervisorEvent::ReconnectFailed {
                server: reported,
                error,
                failures,
                retry_in,
            },
        ] => {
            assert_eq!(
                *dropped,
                SupervisorEvent::TransportDropped {
                    server: ServerRef::from(&server),
                    outcome: ProbeOutcome::TimedOut {
                        after: TEST_PROBE_TIMEOUT,
                    },
                    consecutive_timeouts: 3,
                }
            );
            assert_eq!(*reported, ServerRef::from(&server));
            assert!(error.contains("not accepting sessions"), "{error}");
            assert_eq!(*failures, 1);
            assert_eq!(*retry_in, BACKOFF_BASE);
        }
        other => panic!("expected a drop then a refused reconnect, got {other:?}"),
    }
}

#[tokio::test]
async fn a_broken_transport_is_reported_and_so_is_the_rebuild_that_follows() {
    // The common field case: one request fails, the session is rebuilt within
    // the same cycle, and no user was around to notice. The report still
    // carries both halves, the drop first, with `after_failures` at zero so a
    // host can tell this blip from a server that stayed down.
    let dials = ServerDials::new();
    let url = serve_adjustable_server(&dials).await;
    let (store, connections, oauth, server) = connected_to(url).await;
    dials.fail_next_list();
    let mut supervisor = probing_supervisor();

    let report = supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;

    assert_eq!(kinds(&report), ["transport_dropped", "reconnected"]);
    match report.events.as_slice() {
        [
            SupervisorEvent::TransportDropped {
                server: reported,
                outcome: ProbeOutcome::Broken { error, .. },
                consecutive_timeouts,
            },
            rebuilt,
        ] => {
            assert_eq!(*reported, ServerRef::from(&server));
            assert!(error.contains("the session is gone"), "{error}");
            assert_eq!(*consecutive_timeouts, 0);
            assert_eq!(
                *rebuilt,
                SupervisorEvent::Reconnected {
                    server: ServerRef::from(&server),
                    tools: 1,
                    after_failures: 0,
                }
            );
        }
        other => panic!("expected a broken drop then a rebuild, got {other:?}"),
    }
    assert_eq!(connections.connected_count().await, 1);
}

#[tokio::test]
async fn a_failed_reconnect_is_reported_with_its_penalty_and_the_window_is_quiet() {
    let store = Store::open_in_memory().unwrap();
    let server = install(
        "srv-1",
        Transport::HttpRemote {
            url: "http://127.0.0.1:1/mcp".into(),
        },
        true,
    );
    store.insert_server(&server).unwrap();
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();
    let base = Instant::now();

    let report = supervisor.tick(&store, &connections, &oauth, base).await;

    match report.events.as_slice() {
        [
            SupervisorEvent::ReconnectFailed {
                server: reported,
                failures,
                retry_in,
                ..
            },
        ] => {
            assert_eq!(*reported, ServerRef::from(&server));
            assert_eq!(*failures, 1);
            assert_eq!(*retry_in, BACKOFF_BASE);
        }
        other => panic!("expected one reconnect_failed event, got {other:?}"),
    }

    // Inside the backoff window nothing is dialled, so there is nothing to
    // report — a host must not read silence as recovery.
    let quiet = supervisor
        .tick(&store, &connections, &oauth, base + Duration::from_secs(1))
        .await;
    assert!(quiet.is_empty());
}

#[tokio::test]
async fn a_reconnect_after_failures_reports_how_many_it_took() {
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

    // The server comes back, and the window has elapsed.
    store.delete_server("srv-1").unwrap();
    let server = install("srv-1", Transport::HttpRemote { url }, true);
    store.insert_server(&server).unwrap();
    let report = supervisor
        .tick(&store, &connections, &oauth, base + Duration::from_secs(30))
        .await;

    assert_eq!(
        report.events,
        [SupervisorEvent::Reconnected {
            server: ServerRef::from(&server),
            tools: 1,
            after_failures: 1,
        }]
    );
}

#[tokio::test]
async fn a_parked_server_is_reported_once_and_then_stays_quiet() {
    let store = Store::open_in_memory().unwrap();
    let server = install("srv-1", Transport::Stdio, true);
    store.insert_server(&server).unwrap();
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([(
                "PATH".to_string(),
                "/tinymcp/deliberately/does/not/exist".to_string(),
            )]),
        )
        .unwrap();
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();
    let base = Instant::now();

    let report = supervisor.tick(&store, &connections, &oauth, base).await;

    match report.events.as_slice() {
        [
            SupervisorEvent::Parked {
                server: reported,
                error,
            },
        ] => {
            assert_eq!(*reported, ServerRef::from(&server));
            assert!(!error.is_empty(), "the parking reason is carried");
        }
        other => panic!("expected one parked event, got {other:?}"),
    }

    let quiet = supervisor
        .tick(&store, &connections, &oauth, base + BACKOFF_MAX * 2)
        .await;
    assert!(
        quiet.is_empty(),
        "a parked server is not retried, so there is nothing to report"
    );
}

#[tokio::test]
async fn a_disabled_install_and_an_empty_store_report_nothing() {
    let store = Store::open_in_memory().unwrap();
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    let mut supervisor = supervisor();

    let empty = supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;
    assert!(empty.is_empty());

    store
        .insert_server(&install("srv-off", Transport::Stdio, false))
        .unwrap();
    let disabled = supervisor
        .tick(&store, &connections, &oauth, Instant::now())
        .await;
    assert!(disabled.is_empty());
}

#[test]
fn a_server_ref_carries_the_install_identity() {
    let server = install("srv-1", Transport::Stdio, true);
    let reference = ServerRef::from(&server);

    assert_eq!(reference.server_id, "srv-1");
    assert_eq!(reference.qualified_name, "@test/srv-1");
    assert_eq!(reference.display_name, "srv-1");
}

#[test]
fn every_event_names_its_server_and_its_kind() {
    let server = ServerRef {
        server_id: "srv-1".into(),
        qualified_name: "@test/srv-1".into(),
        display_name: "srv-1".into(),
    };
    let events = [
        SupervisorEvent::ProbeAnswered {
            server: server.clone(),
            elapsed: Duration::from_millis(5),
        },
        SupervisorEvent::ProbeTimedOut {
            server: server.clone(),
            after: Duration::from_secs(8),
            consecutive: 1,
            teardown_after: 3,
        },
        SupervisorEvent::TransportDropped {
            server: server.clone(),
            outcome: ProbeOutcome::Missing,
            consecutive_timeouts: 0,
        },
        SupervisorEvent::Reconnected {
            server: server.clone(),
            tools: 2,
            after_failures: 0,
        },
        SupervisorEvent::ReconnectFailed {
            server: server.clone(),
            error: "refused".into(),
            failures: 1,
            retry_in: Duration::from_secs(5),
        },
        SupervisorEvent::Parked {
            server: server.clone(),
            error: "no runtime".into(),
        },
    ];

    let labels: Vec<_> = events.iter().map(SupervisorEvent::kind).collect();
    assert_eq!(
        labels,
        [
            "probe_answered",
            "probe_timed_out",
            "transport_dropped",
            "reconnected",
            "reconnect_failed",
            "parked",
        ]
    );
    for event in &events {
        assert_eq!(event.server(), &server);
    }
    assert!(TickReport::default().is_empty());
}
