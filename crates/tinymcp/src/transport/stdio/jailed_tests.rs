//! The jailed launcher drives a fake `TinyBox` over the bus seam.
//!
//! The fake answers the same members the module serves and records every call,
//! so the tests pin what is sent (grants, enforcement, argv, environment), what
//! is refused, and what is cleaned up.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::transport::stdio::McpStdioClient;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;
use tinymcp_bus::{LATEST_PROTOCOL_VERSION, McpClientIdentityConfig};

#[derive(Debug, Default)]
struct State {
    calls: Vec<(&'static str, Value)>,
    stdout: VecDeque<u8>,
    written: Vec<u8>,
    closed: bool,
    start_error: Option<&'static str>,
    contract: Option<(u32, u32)>,
    next: u32,
    behavior: Behavior,
    /// Members called over the confidential route.
    confidential: Vec<&'static str>,
    /// Reads called over the confidential route (reads are not in `calls`).
    confidential_reads: usize,
}

/// How much of the jail the fake backend can enforce.
#[derive(Debug, Default, PartialEq, Eq)]
enum Backend {
    #[default]
    Full,
    BestEffort,
}

/// What the fake does differently from a plain, silent module.
#[derive(Debug, Default)]
struct Behavior {
    /// Reply as an MCP server to each line written to it.
    respond: bool,
    /// Make `Create` fail.
    create_error: bool,
    /// Act as a best-effort backend: refuse `StartStdio` unless the jail
    /// asks for `declared` enforcement.
    backend: Backend,
    /// Report stdout closed while stderr stays open.
    stdout_only_closed: bool,
}

#[derive(Debug, Default)]
struct FakeBox {
    state: Mutex<State>,
    wake: tokio::sync::Notify,
    close_stdin_gate: Mutex<Option<Arc<CloseStdinGate>>>,
}

impl FakeBox {
    fn serving() -> Arc<Self> {
        let fake = Self::default();
        fake.state.lock().unwrap().behavior.respond = true;
        Arc::new(fake)
    }
    fn serving_with_close_stdin_gate(gate: Arc<CloseStdinGate>) -> Arc<Self> {
        let fake = Self::serving();
        *fake.close_stdin_gate.lock().unwrap() = Some(gate);
        fake
    }
    fn members(&self) -> Vec<&'static str> {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|c| c.0)
            .collect()
    }
    fn args(&self, member: &str) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|call| call.0 == member)
            .map(|call| call.1.clone())
            .collect()
    }
}

impl JailBus for FakeBox {
    fn call(
        &self,
        member: &'static str,
        args: Value,
        confidential: bool,
    ) -> BoxFuture<'_, Result<Value, JailBusError>> {
        let close_stdin_gate = self.close_stdin_gate.lock().unwrap().clone();
        Box::pin(async move {
            // Positional array in, as the bus delivers it; the fake works on
            // the single argument.
            assert!(args.is_array(), "arguments travel positionally");
            let args = args.get(0).cloned().unwrap_or(Value::Null);
            if member == names::READ_STDIO {
                if confidential {
                    self.state.lock().unwrap().confidential_reads += 1;
                }
                // Long-poll like the module: wait briefly for output.
                let _ = tokio::time::timeout(Duration::from_millis(20), self.wake.notified()).await;
                let mut state = self.state.lock().unwrap();
                let out: Vec<u8> = state.stdout.drain(..).collect();
                let closed = state.closed && state.stdout.is_empty();
                let stdout_closed = closed || state.behavior.stdout_only_closed;
                return Ok(json!({
                    "stdout": out, "stderr": [], "stdout_closed": stdout_closed,
                    "stderr_closed": closed, "status": {"running": !closed}
                }));
            }
            gate_before_fakebox_record(close_stdin_gate.as_deref(), member);
            let mut state = self.state.lock().unwrap();
            state.calls.push((member, args.clone()));
            gate_after_fakebox_record(close_stdin_gate.as_deref(), member);
            if confidential {
                state.confidential.push(member);
            }
            match member {
                m if m == names::CAPABILITIES => Ok(json!({
                    "contract_version": state.contract.unwrap_or(tinybox_bus::CONTRACT_VERSION),
                    "create_backends": [], "exec_backends": [], "spawn_backends": []
                })),
                m if m == names::RESERVE => {
                    state.next += 1;
                    Ok(json!(format!("id-{}", state.next)))
                }
                m if m == names::CREATE && state.behavior.create_error => Err(JailBusError {
                    name: Some(tinybox_bus::INVALID_FILE_PATH.into()),
                    message: "bad workspace".into(),
                }),
                m if m == names::CREATE => Ok(json!({
                    "resource": args["resource"], "backend": "passthrough",
                    "state": "running", "published_ports": []
                })),
                m if m == names::START_STDIO => {
                    if state.behavior.backend == Backend::BestEffort
                        && args["jail"]["enforcement"] != "declared"
                    {
                        return Err(JailBusError {
                            name: Some(tinybox_bus::UNSUPPORTED_OPERATION.into()),
                            message: "backend cannot enforce every grant".into(),
                        });
                    }
                    if let Some(name) = state.start_error {
                        return Err(JailBusError {
                            name: Some(name.into()),
                            message: "refused".into(),
                        });
                    }
                    Ok(json!({
                        "resource": args["resource"], "process": args["process"]
                    }))
                }
                m if m == names::WRITE_STDIO => {
                    let data: Vec<u8> = serde_json::from_value(args["data"].clone()).unwrap();
                    state.written.extend_from_slice(&data);
                    if state.behavior.respond {
                        let text = String::from_utf8_lossy(&data).into_owned();
                        for line in text.lines() {
                            let request: Value = serde_json::from_str(line).unwrap();
                            if let Some(id) = request.get("id") {
                                let result = match request["method"].as_str() {
                                    Some("initialize") => json!({
                                        "protocolVersion": LATEST_PROTOCOL_VERSION,
                                        "capabilities": {}, "serverInfo": {"name": "fake"}
                                    }),
                                    _ => json!({"tools": []}),
                                };
                                let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
                                state.stdout.extend(format!("{reply}\n").into_bytes());
                            }
                        }
                        self.wake.notify_waiters();
                    }
                    Ok(Value::Null)
                }
                m if m == names::CLOSE_STDIN || m == names::KILL_STDIO => {
                    state.closed = true;
                    self.wake.notify_waiters();
                    Ok(Value::Null)
                }
                _ => Ok(Value::Null),
            }
        })
    }
}

fn gate_before_fakebox_record(gate: Option<&CloseStdinGate>, member: &'static str) {
    if member == names::CLOSE_STDIN
        && let Some(gate) = gate
    {
        gate.enter_and_wait();
    }
}

fn gate_after_fakebox_record(gate: Option<&CloseStdinGate>, member: &'static str) {
    if member == names::CLOSE
        && let Some(gate) = gate
    {
        gate.close_recorded();
    }
    if member == names::CLOSE_STDIN
        && let Some(gate) = gate
    {
        gate.stdin_close_recorded();
    }
}

#[derive(Debug)]
struct CloseStdinGate {
    entered: std::sync::mpsc::SyncSender<()>,
    allow_record: Mutex<std::sync::mpsc::Receiver<()>>,
    close_recorded: std::sync::mpsc::SyncSender<()>,
    stdin_recorded: std::sync::mpsc::SyncSender<()>,
    deadline: Mutex<Option<std::time::Instant>>,
}

impl CloseStdinGate {
    fn new() -> (Arc<Self>, CloseStdinGateObserver) {
        let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (allow_record, allow_record_rx) = std::sync::mpsc::sync_channel(1);
        let (close_recorded, close_recorded_rx) = std::sync::mpsc::sync_channel(1);
        let (stdin_recorded, stdin_recorded_rx) = std::sync::mpsc::sync_channel(1);
        (
            Arc::new(Self {
                entered,
                allow_record: Mutex::new(allow_record_rx),
                close_recorded,
                stdin_recorded,
                deadline: Mutex::new(None),
            }),
            CloseStdinGateObserver {
                entered: Mutex::new(Some(entered_rx)),
                allow_record,
                close_recorded: close_recorded_rx,
                stdin_recorded: stdin_recorded_rx,
            },
        )
    }

    fn set_deadline(&self, deadline: std::time::Instant) {
        *self.deadline.lock().unwrap() = Some(deadline);
    }

    fn remaining(&self) -> Duration {
        self.deadline
            .lock()
            .unwrap()
            .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap_or_default()
    }

    fn enter_and_wait(&self) {
        let _ = self.entered.send(());
        let _ = self
            .allow_record
            .lock()
            .unwrap()
            .recv_timeout(self.remaining());
    }

    fn close_recorded(&self) {
        let _ = self.close_recorded.try_send(());
    }

    fn stdin_close_recorded(&self) {
        let _ = self.stdin_recorded.try_send(());
    }
}

struct CloseStdinGateObserver {
    entered: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    allow_record: std::sync::mpsc::SyncSender<()>,
    close_recorded: std::sync::mpsc::Receiver<()>,
    stdin_recorded: std::sync::mpsc::Receiver<()>,
}

impl CloseStdinGateObserver {
    fn take_entered(&self) -> std::sync::mpsc::Receiver<()> {
        self.entered
            .lock()
            .unwrap()
            .take()
            .expect("the entry receiver is taken once")
    }

    fn release_stdin_gate(&self) {
        let _ = self.allow_record.try_send(());
    }
}

impl Drop for CloseStdinGateObserver {
    fn drop(&mut self) {
        self.release_stdin_gate();
    }
}

fn wait_for_jail_cleanup(fake: &FakeBox, deadline: std::time::Instant) -> Vec<&'static str> {
    loop {
        let members = fake.members();
        if let Some(close) = members.iter().position(|member| *member == names::CLOSE) {
            let Some(kill) = members
                .iter()
                .position(|member| *member == names::KILL_STDIO)
            else {
                panic!("cleanup recorded Close without KillStdio: {members:?}");
            };
            assert!(kill < close, "cleanup calls are out of order: {members:?}");
            return members;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "cleanup did not record Close before the deadline: {members:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[path = "jailed_tests/cleanup_discriminator_tests.rs"]
mod cleanup_discriminator_tests;

fn sandbox() -> StdioSandboxConfig {
    StdioSandboxConfig {
        workspace: "/work/mcp".into(),
        read_only: vec!["/usr".into()],
        read_write: vec!["/work/mcp/cache".into()],
        deny_read: vec!["/home/user/.ssh".into()],
        allow_net: true,
        allow_subprocess: false,
        require_process_isolation: true,
        enforcement: StdioSandboxEnforcement::default(),
    }
}

fn launch() -> StdioLaunch {
    StdioLaunch {
        program: "/usr/bin/server".into(),
        args: vec!["--stdio".into()],
        env: vec![("TOKEN".into(), "secret-value".into())],
        cwd: Some("/work/mcp/run".into()),
    }
}

#[tokio::test]
async fn a_handshake_runs_through_the_jail_with_the_hosts_grants() {
    let fake = FakeBox::serving();
    let spawner = Arc::new(JailedSpawner::new(fake.clone(), sandbox()));
    let client = McpStdioClient::new(
        "sh",
        vec![],
        vec![],
        None,
        &McpClientIdentityConfig::default(),
    )
    .with_spawner(spawner);
    client.initialize().await.expect("handshake over the jail");
    assert_eq!(client.list_tools().await.unwrap().len(), 0);

    let members = fake.members();
    assert_eq!(
        &members[..5],
        [
            names::CAPABILITIES,
            names::RESERVE,
            names::CREATE,
            names::RESERVE,
            names::START_STDIO
        ]
    );
    let create = &fake.args(names::CREATE)[0];
    assert_eq!(create["backend"], "passthrough");
    assert_eq!(create["workspace"], json!({"Directory": "/work/mcp"}));
    let start = &fake.args(names::START_STDIO)[0];
    assert_eq!(start["jail"]["enforcement"], "enforced");
    assert_eq!(start["jail"]["read_only"], json!(["/usr"]));
    assert_eq!(start["jail"]["read_write"], json!(["/work/mcp/cache"]));
    assert_eq!(start["jail"]["deny_read"], json!(["/home/user/.ssh"]));
    assert_eq!(start["jail"]["allow_net"], true);
    assert_eq!(start["jail"]["allow_subprocess"], false);
    assert_eq!(start["jail"]["require_process_isolation"], true);

    client.close_session().await.unwrap();
    let members = fake.members();
    assert!(members.contains(&names::KILL_STDIO));
    assert_eq!(members.last(), Some(&names::CLOSE));
}

#[test]
fn the_sandbox_enforcement_defaults_to_enforced() {
    assert_eq!(
        StdioSandboxEnforcement::default(),
        StdioSandboxEnforcement::Enforced
    );
    assert_eq!(sandbox().enforcement, StdioSandboxEnforcement::Enforced);
}

#[tokio::test]
async fn declared_enforcement_lets_a_best_effort_backend_start() {
    let fake = FakeBox::serving();
    fake.state.lock().unwrap().behavior.backend = Backend::BestEffort;
    let config = StdioSandboxConfig {
        enforcement: StdioSandboxEnforcement::Declared,
        ..sandbox()
    };
    let spawner = JailedSpawner::new(fake.clone(), config);
    let mut process = spawner.spawn_process(launch()).await.unwrap();
    let start = &fake.args(names::START_STDIO)[0];
    assert_eq!(start["jail"]["enforcement"], "declared");
    process.handle.terminate().await;
}

#[tokio::test]
async fn enforced_enforcement_still_refuses_a_best_effort_backend() {
    let fake = FakeBox::serving();
    fake.state.lock().unwrap().behavior.backend = Backend::BestEffort;
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let error = spawner.spawn_process(launch()).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(fake.members().last(), Some(&names::CLOSE));
}

#[tokio::test]
async fn launch_arguments_reach_the_jailed_start_unchanged() {
    let fake = FakeBox::serving();
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let mut process = spawner.spawn_process(launch()).await.unwrap();
    let start = &fake.args(names::START_STDIO)[0];
    assert_eq!(start["argv"], json!(["/usr/bin/server", "--stdio"]));
    assert_eq!(start["cwd"], "/work/mcp/run");
    assert_eq!(start["env"], json!({"TOKEN": "secret-value"}));
    process.handle.terminate().await;
}

#[tokio::test]
async fn a_refused_start_is_a_permission_error_and_closes_the_resource() {
    for name in [
        tinybox_bus::UNSUPPORTED_OPERATION,
        tinybox_bus::INVALID_FILE_PATH,
    ] {
        let fake = FakeBox::serving();
        fake.state.lock().unwrap().start_error = Some(name);
        let spawner = JailedSpawner::new(fake.clone(), sandbox());
        let error = spawner.spawn_process(launch()).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        let members = fake.members();
        assert_eq!(members.last(), Some(&names::CLOSE));
        assert!(!members.contains(&names::KILL_STDIO));
    }
}

#[tokio::test]
async fn another_start_failure_is_not_reported_as_a_policy_refusal() {
    let fake = FakeBox::serving();
    fake.state.lock().unwrap().start_error = Some(tinybox_bus::BACKEND_ERROR);
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let error = spawner.spawn_process(launch()).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
}

#[tokio::test]
async fn a_sandbox_module_too_old_for_stdio_is_refused_before_any_resource_exists() {
    let fake = FakeBox::serving();
    fake.state.lock().unwrap().contract = Some((1, 20));
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let error = spawner.spawn_process(launch()).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(fake.members(), [names::CAPABILITIES]);
}

#[tokio::test]
async fn the_jailed_spawner_never_produces_a_child_process() {
    let spawner = JailedSpawner::new(FakeBox::serving(), sandbox());
    let error = spawner.spawn(launch()).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}

#[tokio::test]
async fn dropping_a_started_server_releases_it_in_the_background() {
    let fake = FakeBox::serving();
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let process = spawner.spawn_process(launch()).await.unwrap();
    drop(process);
    tokio::time::timeout(Duration::from_secs(5), async {
        while fake.members().last() != Some(&names::CLOSE) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the dropped server was killed and its resource closed");
    assert!(fake.members().contains(&names::KILL_STDIO));
}

#[tokio::test]
async fn client_writes_are_forwarded_and_end_of_input_closes_stdin() {
    let fake = FakeBox::serving();
    fake.state.lock().unwrap().behavior.respond = false;
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let mut process = spawner.spawn_process(launch()).await.unwrap();
    process.stdin.write_all(b"ping\n").await.unwrap();
    process.stdin.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fake.members().contains(&names::CLOSE_STDIN) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("stdin was closed");
    assert_eq!(fake.state.lock().unwrap().written, b"ping\n");
    // The fake reports end of file once stdin closed, so the reader ends.
    let mut rest = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        process.stdout.read_to_end(&mut rest),
    )
    .await
    .expect("stdout reaches end of file")
    .unwrap();
    assert_eq!(rest.len(), 0);
    process.handle.terminate().await;
}

#[tokio::test]
async fn an_untrusted_server_is_refused_when_no_sandbox_is_configured() {
    let launchers = StdioLaunchers::new();
    assert!(!launchers.has_sandbox());
    let error = launchers
        .for_server(false)
        .spawn_process(launch())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("without a sandbox"), "{error}");
}

#[tokio::test]
async fn an_untrusted_server_uses_the_sandbox_and_never_the_direct_spawn() {
    let fake = FakeBox::serving();
    let launchers =
        StdioLaunchers::sandboxed(Arc::new(JailedSpawner::new(fake.clone(), sandbox())));
    assert!(launchers.has_sandbox());
    let mut process = launchers
        .for_server(false)
        .spawn_process(launch())
        .await
        .unwrap();
    assert!(fake.members().contains(&names::START_STDIO));
    process.handle.terminate().await;
}

#[cfg(unix)]
#[tokio::test]
async fn only_an_operator_trusted_server_gets_the_direct_spawn() {
    let fake = FakeBox::serving();
    let launchers =
        StdioLaunchers::sandboxed(Arc::new(JailedSpawner::new(fake.clone(), sandbox())));
    let mut process = launchers
        .for_server(true)
        .spawn_process(StdioLaunch {
            program: "/bin/cat".into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
        })
        .await
        .unwrap();
    assert!(
        fake.members().is_empty(),
        "a trusted server bypasses the jail"
    );
    process.handle.terminate().await;
    // With no sandbox at all, trust still starts it directly.
    let mut process = StdioLaunchers::new()
        .for_server(true)
        .spawn_process(StdioLaunch {
            program: "/bin/cat".into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
        })
        .await
        .unwrap();
    process.handle.terminate().await;
}

#[tokio::test]
async fn the_environment_and_the_messages_sent_to_the_server_are_confidential() {
    let fake = FakeBox::serving();
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let mut process = spawner.spawn_process(launch()).await.unwrap();
    assert_eq!(
        fake.state.lock().unwrap().confidential,
        [names::START_STDIO]
    );
    process.handle.terminate().await;
}

#[tokio::test]
async fn a_failed_create_closes_the_reserved_resource() {
    let fake = FakeBox::serving();
    fake.state.lock().unwrap().behavior.create_error = true;
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let error = spawner.spawn_process(launch()).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let members = fake.members();
    assert_eq!(members.last(), Some(&names::CLOSE));
    assert!(!members.contains(&names::START_STDIO));
}

#[tokio::test]
async fn the_reader_ends_when_stdout_closes_even_if_stderr_stays_open() {
    let fake = FakeBox::serving();
    fake.state.lock().unwrap().behavior.respond = false;
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let mut process = spawner.spawn_process(launch()).await.unwrap();
    // The fake reports both streams closed together; make only stdout close.
    fake.state.lock().unwrap().behavior.stdout_only_closed = true;
    let mut rest = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        process.stdout.read_to_end(&mut rest),
    )
    .await
    .expect("stdout reaches end of file without stderr closing")
    .unwrap();
    process.handle.terminate().await;
}

/// A stand-in `TinyBox` served on an in-process broker, to drive the real
/// bus-backed [`BusJail`].
struct ServedBox;

#[tinybus::interface(name = "ai.tinyhumans.tinybox.Box")]
impl ServedBox {
    async fn capabilities(&self) -> tinybus::Result<tinybox_bus::ModuleCapabilities> {
        std::future::ready(Ok(tinybox_bus::ModuleCapabilities {
            contract_version: tinybox_bus::CONTRACT_VERSION,
            create_backends: vec!["passthrough".into()],
            exec_backends: Vec::new(),
            spawn_backends: Vec::new(),
        }))
        .await
    }

    async fn reserve(
        &self,
        _request: tinybox_bus::ReserveRequest,
    ) -> tinybus::Result<tinybox_bus::ResourceId> {
        std::future::ready(Err(tinybus::Error::MethodFailed {
            name: tinybox_bus::UNSUPPORTED_OPERATION.into(),
            message: "no jail here".into(),
        }))
        .await
    }
}

#[cfg(feature = "module")]
#[tokio::test]
async fn the_bus_backed_jail_calls_tinybox_and_keeps_its_error_names() {
    use tinybus::broker::Broker;
    use tinybus::transport::memory::MemoryBus;
    let bus = MemoryBus::new();
    Broker::new().spawn(bus.clone());
    let serving = tinybus::Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    serving
        .serve_at(tinybox_bus::OBJECT_PATH.try_into().unwrap(), ServedBox)
        .await
        .unwrap();
    serving.request_name(tinybox_bus::INTERFACE).await.unwrap();
    let client = tinybus::Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    let jail = BusJail::connect(&client).unwrap();

    let reply = jail
        .call(names::CAPABILITIES, json!([]), false)
        .await
        .unwrap();
    assert_eq!(
        reply["contract_version"],
        json!([
            tinybox_bus::CONTRACT_VERSION.0,
            tinybox_bus::CONTRACT_VERSION.1
        ])
    );

    let refused = jail
        .call(names::RESERVE, json!(["Resource"]), false)
        .await
        .unwrap_err();
    assert_eq!(
        refused.name.as_deref(),
        Some(tinybox_bus::UNSUPPORTED_OPERATION)
    );

    // Credential-bearing calls need an attested destination; an in-process
    // stand-in is not one, so the confidential route refuses instead of
    // quietly sending the environment the ordinary way.
    let unattested = jail
        .call(names::START_STDIO, json!([{}]), true)
        .await
        .unwrap_err();
    assert_eq!(
        unattested.name.as_deref(),
        Some("ai.tinyhumans.tinybus.Error.NotAttested")
    );

    let spawner = JailedSpawner::new(Arc::new(jail), sandbox());
    let error = spawner.spawn_process(launch()).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}

#[tokio::test]
async fn a_started_handle_has_a_redacted_debug_form() {
    let fake = FakeBox::serving();
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let mut process = spawner.spawn_process(launch()).await.unwrap();
    let debug = format!("{:?}", process.handle);
    assert!(!debug.contains("secret-value"));
    process.handle.terminate().await;
}

/// Parks a call forever, as a bus whose reply never comes.
#[derive(Debug, Default)]
struct Stalled {
    calls: Mutex<Vec<&'static str>>,
}

impl JailBus for Stalled {
    fn call(
        &self,
        member: &'static str,
        args: Value,
        _confidential: bool,
    ) -> BoxFuture<'_, Result<Value, JailBusError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(member);
            let _ = args;
            match member {
                m if m == names::CAPABILITIES => Ok(json!({
                    "contract_version": tinybox_bus::CONTRACT_VERSION,
                    "create_backends": [], "exec_backends": [], "spawn_backends": []
                })),
                m if m == names::RESERVE => Ok(json!("reserved")),
                m if m == names::CREATE => std::future::pending().await,
                _ => Ok(Value::Null),
            }
        })
    }
}

#[tokio::test]
async fn a_launch_dropped_while_create_is_pending_still_closes_its_reservation() {
    let bus = Arc::new(Stalled::default());
    let spawner = JailedSpawner::new(bus.clone(), sandbox());
    let launching =
        tokio::time::timeout(Duration::from_millis(100), spawner.spawn_process(launch()));
    assert!(launching.await.is_err(), "the Create never answers");
    tokio::time::timeout(Duration::from_secs(5), async {
        while bus.calls.lock().unwrap().last() != Some(&names::CLOSE) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the abandoned reservation was closed");
}

#[test]
fn dropping_a_handle_with_no_runtime_still_releases_the_server() {
    let fake = FakeBox::serving();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let process = runtime.block_on(spawner.spawn_process(launch())).unwrap();
    // Dropped on a plain thread: there is no current runtime to spawn on.
    std::thread::spawn(move || drop(process)).join().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let members = wait_for_jail_cleanup(&fake, deadline);
    assert!(members.contains(&names::KILL_STDIO));
}

#[tokio::test]
async fn reads_are_confidential_too() {
    let fake = FakeBox::serving();
    let spawner = JailedSpawner::new(fake.clone(), sandbox());
    let mut process = spawner.spawn_process(launch()).await.unwrap();
    // The fake does not record reads in `calls`, so ask for one directly.
    let mut byte = [0_u8; 1];
    let _ = tokio::time::timeout(Duration::from_millis(100), process.stdout.read(&mut byte)).await;
    assert!(fake.state.lock().unwrap().confidential_reads > 0);
    process.handle.terminate().await;
}

/// Answers everything, except that the first `KillStdio` never returns.
#[derive(Debug, Default)]
struct StallsFirstKill {
    calls: Mutex<Vec<&'static str>>,
}

impl JailBus for StallsFirstKill {
    fn call(
        &self,
        member: &'static str,
        args: Value,
        _confidential: bool,
    ) -> BoxFuture<'_, Result<Value, JailBusError>> {
        Box::pin(async move {
            let first_kill = {
                let mut calls = self.calls.lock().unwrap();
                let first = member == names::KILL_STDIO && !calls.contains(&names::KILL_STDIO);
                if member != names::READ_STDIO {
                    calls.push(member);
                }
                first
            };
            if first_kill {
                return std::future::pending().await;
            }
            let args = args.get(0).cloned().unwrap_or(Value::Null);
            match member {
                m if m == names::CAPABILITIES => Ok(json!({
                    "contract_version": tinybox_bus::CONTRACT_VERSION,
                    "create_backends": [], "exec_backends": [], "spawn_backends": []
                })),
                m if m == names::RESERVE => Ok(json!("reserved")),
                m if m == names::CREATE => Ok(json!({
                    "resource": args["resource"], "backend": "passthrough",
                    "state": "running", "published_ports": []
                })),
                m if m == names::START_STDIO => Ok(json!({
                    "resource": args["resource"], "process": args["process"]
                })),
                m if m == names::READ_STDIO => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Ok(json!({
                        "stdout": [], "stderr": [], "stdout_closed": false,
                        "stderr_closed": false, "status": {"running": true}
                    }))
                }
                _ => Ok(Value::Null),
            }
        })
    }
}

#[tokio::test]
async fn a_cancelled_close_still_releases_the_server_on_drop() {
    let bus = Arc::new(StallsFirstKill::default());
    let spawner = JailedSpawner::new(bus.clone(), sandbox());
    let mut process = spawner.spawn_process(launch()).await.unwrap();
    let cancelled =
        tokio::time::timeout(Duration::from_millis(100), process.handle.terminate()).await;
    assert!(cancelled.is_err(), "the first KillStdio never answers");
    drop(process);
    tokio::time::timeout(Duration::from_secs(5), async {
        while bus.calls.lock().unwrap().last() != Some(&names::CLOSE) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the drop fallback killed and closed the server");
    let calls = bus.calls.lock().unwrap();
    assert_eq!(
        calls.iter().filter(|m| **m == names::KILL_STDIO).count(),
        2,
        "the cancelled kill was retried by the drop"
    );
}
