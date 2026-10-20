//! Starting stdio MCP servers inside `TinyBox`'s native jail, over the bus.
//!
//! [`JailedSpawner`] is a [`StdioSpawner`] whose servers do not run as children
//! of this process. It reserves a passthrough resource whose workspace is the
//! host's [`StdioSandboxConfig::workspace`], starts the server through the
//! `StartStdio` member of the `TinyBox` bus contract with the host's grants, and
//! pumps its stdin and stdout over `WriteStdio` / `ReadStdio`. `TinyBox` refuses
//! when the backend cannot enforce the grants, and that refusal is the result:
//! there is no fallback to an unconfined start.
//!
//! [`StdioLaunchers`] picks the launcher for a server: the direct spawn only
//! for an operator-trusted server, the jail when the host configured one, and
//! otherwise a refusal.
//!
//! Only the `tinybox-bus` contract is used. The call surface is the small
//! [`JailBus`] seam, so tests substitute a fake and the real implementation is
//! one bus proxy.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tinybox_bus::{
    CreateRequest, JailEnforcement, NativeJailConfig, ProcessRef, ReserveRequest, ResourceId,
    StdioChunk, StdioReadRequest, StdioStartRequest, StdioWriteRequest, Workspace, names,
};
use tinymcp_bus::{StdioSandboxConfig, StdioSandboxEnforcement};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::task::JoinHandle;

use super::{DirectSpawner, StdioHandle, StdioLaunch, StdioProcess, StdioSpawner};

/// How long one `ReadStdio` may wait; well under the bus call deadline.
const READ_WAIT_MS: u64 = 1_000;
/// The in-memory pipe between a pump task and the client.
const PIPE_BYTES: usize = 64 * 1024;

/// Why a bus call failed: the member's stable error name, when it had one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JailBusError {
    /// The stable error name `TinyBox` returned, such as
    /// `ai.tinyhumans.tinybox.Error.UnsupportedOperation`.
    pub name: Option<String>,
    /// A message without request content.
    pub message: String,
}

/// The one call surface the launcher needs from the `TinyBox` module.
///
/// Arguments and results cross as JSON, which is how the bus carries them.
/// `args` is the positional argument array. A bus-backed implementation is a
/// proxy to `ai.tinyhumans.tinybox.Box`.
pub trait JailBus: Send + Sync + std::fmt::Debug {
    /// Calls `member` with the positional `args` and returns the reply.
    ///
    /// `confidential` asks for the bus's confidential delivery, which only an
    /// attested module receives. A call whose arguments carry credentials sets
    /// it, and fails rather than falls back when delivery cannot be confidential.
    fn call(
        &self,
        member: &'static str,
        args: Value,
        confidential: bool,
    ) -> BoxFuture<'_, Result<Value, JailBusError>>;
}

/// [`JailBus`] over a `TinyBus` connection to the `TinyBox` module.
#[cfg(feature = "module")]
#[derive(Debug, Clone)]
pub struct BusJail {
    proxy: tinybus::Proxy,
}

#[cfg(feature = "module")]
impl BusJail {
    /// Binds to the `TinyBox` object on `connection`.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the contract's names are not valid.
    pub fn connect(connection: &tinybus::Connection) -> Result<Self, tinybus::Error> {
        Ok(Self {
            proxy: connection.proxy(
                tinybox_bus::INTERFACE,
                tinybox_bus::OBJECT_PATH,
                tinybox_bus::INTERFACE,
            )?,
        })
    }
}

#[cfg(feature = "module")]
impl JailBus for BusJail {
    fn call(
        &self,
        member: &'static str,
        args: Value,
        confidential: bool,
    ) -> BoxFuture<'_, Result<Value, JailBusError>> {
        Box::pin(async move {
            let reply = if confidential {
                self.proxy.call_confidential::<Value>(member, args).await
            } else {
                self.proxy.call::<Value>(member, args).await
            };
            reply.map_err(|error| match error {
                tinybus::Error::MethodFailed { name, message } => JailBusError {
                    name: Some(name),
                    message,
                },
                other => JailBusError {
                    name: None,
                    message: other.to_string(),
                },
            })
        })
    }
}

/// Typed call helper over the JSON seam.
async fn invoke<Reply: DeserializeOwned>(
    bus: &dyn JailBus,
    member: &'static str,
    args: impl Serialize,
) -> Result<Reply, std::io::Error> {
    invoke_with(bus, member, args, false).await
}

/// [`invoke`], optionally over the confidential route.
///
/// Arguments travel as a positional array: nothing for `()`, otherwise the one
/// argument, which is how every `TinyBox` member is declared.
async fn invoke_with<Reply: DeserializeOwned>(
    bus: &dyn JailBus,
    member: &'static str,
    args: impl Serialize,
    confidential: bool,
) -> Result<Reply, std::io::Error> {
    let args = match serde_json::to_value(args).map_err(std::io::Error::other)? {
        Value::Null => Value::Array(Vec::new()),
        one => Value::Array(vec![one]),
    };
    let reply = bus
        .call(member, args, confidential)
        .await
        .map_err(|error| refusal(&error))?;
    serde_json::from_value(reply).map_err(|error| {
        std::io::Error::other(format!(
            "unexpected {member} reply from the sandbox: {error}"
        ))
    })
}

/// A policy refusal is `PermissionDenied`, so callers can tell it from an I/O
/// failure; anything else is `Other`.
fn refusal(error: &JailBusError) -> std::io::Error {
    let denied = error.name.as_deref().is_some_and(|name| {
        name == tinybox_bus::UNSUPPORTED_OPERATION || name == tinybox_bus::INVALID_FILE_PATH
    });
    std::io::Error::new(
        if denied {
            std::io::ErrorKind::PermissionDenied
        } else {
            std::io::ErrorKind::Other
        },
        format!(
            "the sandbox refused or failed the request: {}",
            error.message
        ),
    )
}

/// Starts stdio servers inside the host's jail through `TinyBox`.
#[derive(Debug, Clone)]
pub struct JailedSpawner {
    bus: Arc<dyn JailBus>,
    sandbox: StdioSandboxConfig,
}

impl JailedSpawner {
    /// A launcher that applies `sandbox` through `bus`.
    #[must_use]
    pub fn new(bus: Arc<dyn JailBus>, sandbox: StdioSandboxConfig) -> Self {
        Self { bus, sandbox }
    }

    fn jail(&self) -> NativeJailConfig {
        NativeJailConfig {
            read_only: self.sandbox.read_only.clone(),
            read_write: self.sandbox.read_write.clone(),
            allow_net: self.sandbox.allow_net,
            allow_subprocess: self.sandbox.allow_subprocess,
            // `Enforced` unless the host opted into `Declared`: a partial
            // backend refuses by default rather than weakening the jail.
            enforcement: match self.sandbox.enforcement {
                StdioSandboxEnforcement::Enforced => JailEnforcement::Enforced,
                StdioSandboxEnforcement::Declared => JailEnforcement::Declared,
            },
            require_process_isolation: self.sandbox.require_process_isolation,
            deny_read: self.sandbox.deny_read.clone(),
        }
    }

    async fn launch(&self, launch: StdioLaunch) -> Result<StdioProcess, std::io::Error> {
        let bus = self.bus.as_ref();
        let capabilities: tinybox_bus::ModuleCapabilities =
            invoke(bus, names::CAPABILITIES, ()).await?;
        if !tinybox_bus::is_compatible(capabilities.contract_version) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "the sandbox module is too old for jailed stdio",
            ));
        }
        let resource: ResourceId = invoke(bus, names::RESERVE, ReserveRequest::Resource).await?;
        // Covers a launch dropped mid-await; the error branches below close
        // inline (so the caller sees the cleanup done) and then disarm it.
        let mut guard = ResourceGuard {
            bus: self.bus.clone(),
            resource: resource.clone(),
            armed: true,
        };
        let created: Result<tinybox_bus::ResourceInfo, _> = invoke(
            bus,
            names::CREATE,
            CreateRequest {
                resource: resource.clone(),
                backend: "passthrough".into(),
                workspace: Workspace::Directory(self.sandbox.workspace.clone()),
                ..CreateRequest::default()
            },
        )
        .await;
        if let Err(error) = created {
            // The reservation is not left behind by a refused or malformed Create.
            let _: Result<(), _> = invoke(bus, names::CLOSE, &resource).await;
            guard.armed = false;
            return Err(error);
        }
        let started = self.start(&resource, launch).await;
        if started.is_err() {
            // Nothing runs; the resource must not outlive the refusal.
            let _: Result<(), _> = invoke(bus, names::CLOSE, &resource).await;
        }
        // On success the handle owns the cleanup from here.
        guard.armed = false;
        started
    }

    async fn start(
        &self,
        resource: &ResourceId,
        launch: StdioLaunch,
    ) -> Result<StdioProcess, std::io::Error> {
        let bus = self.bus.as_ref();
        let process: ResourceId = invoke(
            bus,
            names::RESERVE,
            ReserveRequest::Process(resource.clone()),
        )
        .await?;
        let mut argv = vec![launch.program.to_string_lossy().into_owned()];
        argv.extend(launch.args);
        let env: BTreeMap<String, String> = launch.env.into_iter().collect();
        // The environment carries the server's credentials, so this is the
        // call that must not travel on the ordinary route.
        let target: ProcessRef = invoke_with(
            bus,
            names::START_STDIO,
            StdioStartRequest {
                process,
                resource: resource.clone(),
                argv,
                cwd: launch.cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
                env,
                jail: self.jail(),
            },
            true,
        )
        .await?;
        tracing::debug!("started a jailed stdio server");
        Ok(attach(self.bus.clone(), target))
    }
}

impl StdioSpawner for JailedSpawner {
    fn spawn(&self, _launch: StdioLaunch) -> BoxFuture<'_, std::io::Result<tokio::process::Child>> {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "a jailed server is not a child of this process",
            ))
        })
    }

    fn spawn_process(&self, launch: StdioLaunch) -> BoxFuture<'_, std::io::Result<StdioProcess>> {
        Box::pin(self.launch(launch))
    }
}

/// Connects the pipes of a started process to the client.
fn attach(bus: Arc<dyn JailBus>, target: ProcessRef) -> StdioProcess {
    let (stdin_client, stdin_pump) = tokio::io::duplex(PIPE_BYTES);
    let (stdout_client, stdout_pump) = tokio::io::duplex(PIPE_BYTES);
    let writer = tokio::spawn(pump_stdin(bus.clone(), target.clone(), stdin_pump));
    let reader = tokio::spawn(pump_stdout(bus.clone(), target.clone(), stdout_pump));
    StdioProcess {
        stdin: Box::new(stdin_client),
        stdout: Box::new(stdout_client),
        handle: Box::new(JailedHandle {
            bus,
            target,
            tasks: vec![writer, reader],
            released: false,
        }),
    }
}

async fn pump_stdin(bus: Arc<dyn JailBus>, target: ProcessRef, mut pipe: DuplexStream) {
    let mut buffer = vec![0_u8; tinybox_bus::MAX_STDIO_WRITE_BYTES];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) | Err(_) => {
                let _: Result<(), _> = invoke(bus.as_ref(), names::CLOSE_STDIN, &target).await;
                return;
            }
            Ok(read) => {
                let request = StdioWriteRequest {
                    process: target.clone(),
                    data: buffer[..read].to_vec(),
                };
                // The request is an MCP message: tool arguments can carry
                // credentials, so it travels the confidential route too.
                let sent: Result<(), _> =
                    invoke_with(bus.as_ref(), names::WRITE_STDIO, request, true).await;
                if sent.is_err() {
                    // The child is gone; dropping the pipe turns the client's
                    // next write into a broken pipe.
                    return;
                }
            }
        }
    }
}

async fn pump_stdout(bus: Arc<dyn JailBus>, target: ProcessRef, mut pipe: DuplexStream) {
    loop {
        let request = StdioReadRequest {
            process: target.clone(),
            max_bytes: 0,
            wait_ms: READ_WAIT_MS,
        };
        // Replies carry tool results and resource contents, so the read is
        // confidential as well.
        let chunk: StdioChunk =
            match invoke_with(bus.as_ref(), names::READ_STDIO, request, true).await {
                Ok(chunk) => chunk,
                Err(_) => break,
            };
        // Standard error is not protocol and is discarded, as for a direct child.
        if !chunk.stdout.is_empty() && pipe.write_all(&chunk.stdout).await.is_err() {
            return;
        }
        // Standard output is the protocol channel: once it ends nothing more
        // can arrive, whether or not the child keeps standard error open.
        if chunk.stdout_closed {
            break;
        }
    }
    // End of file for the client's reader.
    let _ = pipe.shutdown().await;
}

/// Runs `cleanup` to completion without a caller to wait for it: on the current
/// runtime when there is one, otherwise on a short-lived thread of its own.
///
/// Cleanup after the runtime is gone is best-effort (the bus connection may be
/// closed with it); what it must not do is silently never run.
fn run_detached(cleanup: impl std::future::Future<Output = ()> + Send + 'static) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(cleanup);
        return;
    }
    std::thread::spawn(move || {
        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            runtime.block_on(cleanup);
        }
    });
}

/// Closes a reserved resource unless ownership moved to a [`JailedHandle`],
/// including when the launch future is dropped mid-await.
struct ResourceGuard {
    bus: Arc<dyn JailBus>,
    resource: ResourceId,
    armed: bool,
}

impl Drop for ResourceGuard {
    fn drop(&mut self) {
        if self.armed {
            let (bus, resource) = (self.bus.clone(), self.resource.clone());
            run_detached(async move {
                let _: Result<(), _> = invoke(bus.as_ref(), names::CLOSE, &resource).await;
            });
        }
    }
}

/// Ends a jailed server: kill the group, release the handle, close the
/// resource. Dropping it does the same in the background.
struct JailedHandle {
    bus: Arc<dyn JailBus>,
    target: ProcessRef,
    tasks: Vec<JoinHandle<()>>,
    released: bool,
}

impl std::fmt::Debug for JailedHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JailedHandle").finish_non_exhaustive()
    }
}

async fn release(bus: Arc<dyn JailBus>, target: ProcessRef) {
    let _: Result<(), _> = invoke(bus.as_ref(), names::KILL_STDIO, &target).await;
    let _: Result<(), _> = invoke(bus.as_ref(), names::CLOSE, &target.resource).await;
}

impl StdioHandle for JailedHandle {
    fn terminate(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            for task in &self.tasks {
                task.abort();
            }
            release(self.bus.clone(), self.target.clone()).await;
            // Only now: a close cancelled mid-await leaves this unset, so the
            // drop still runs the cleanup.
            self.released = true;
        })
    }
}

impl Drop for JailedHandle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        if !self.released {
            run_detached(release(self.bus.clone(), self.target.clone()));
        }
    }
}

/// Refuses every start. The fail-closed default when a stdio server is not
/// trusted and the host configured no jail.
#[derive(Debug, Default, Clone, Copy)]
pub struct RefusingSpawner;

impl StdioSpawner for RefusingSpawner {
    fn spawn(&self, _launch: StdioLaunch) -> BoxFuture<'_, std::io::Result<tokio::process::Child>> {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "refusing to start an untrusted stdio server without a sandbox: the host \
                 configured no stdio sandbox and did not mark the server operator-trusted",
            ))
        })
    }
}

/// Chooses how a stdio server is started.
///
/// Cloning is cheap; one value serves every server of a client.
#[derive(Debug, Clone, Default)]
pub struct StdioLaunchers {
    sandboxed: Option<Arc<dyn StdioSpawner>>,
}

impl StdioLaunchers {
    /// No sandbox: only operator-trusted servers can start.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Untrusted servers start through `spawner`.
    #[must_use]
    pub fn sandboxed(spawner: Arc<dyn StdioSpawner>) -> Self {
        Self {
            sandboxed: Some(spawner),
        }
    }

    /// Whether untrusted servers have a jail to start in.
    #[must_use]
    pub fn has_sandbox(&self) -> bool {
        self.sandboxed.is_some()
    }

    /// The launcher for one server.
    ///
    /// `operator_trusted` is the host's explicit statement; it is the only way
    /// to get the unsandboxed direct spawn. Everything else uses the jail, or
    /// is refused when there is none.
    #[must_use]
    pub fn for_server(&self, operator_trusted: bool) -> Arc<dyn StdioSpawner> {
        if operator_trusted {
            Arc::new(DirectSpawner)
        } else if let Some(spawner) = &self.sandboxed {
            spawner.clone()
        } else {
            Arc::new(RefusingSpawner)
        }
    }
}

#[cfg(test)]
#[path = "jailed_tests.rs"]
mod tests;
