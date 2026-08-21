//! The subprocess transport.
//!
//! [`McpStdioClient`] spawns a server as a child process and speaks
//! newline-delimited JSON-RPC over its standard input and output, per the MCP
//! stdio transport.
//!
//! # One session, one child
//!
//! The client holds at most one child at a time, created on the first
//! [`McpStdioClient::initialize`] and reused until
//! [`McpStdioClient::close_session`] or drop. An asynchronous mutex guards it —
//! unlike the HTTP transport's synchronous one, this lock *is* held across
//! awaits, because a request and its reply are a single exchange on one pipe
//! and two interleaved callers would read each other's answers.
//!
//! # The child does not outlive the client
//!
//! The child is spawned with `kill_on_drop`. Without it, a client dropped
//! without an explicit close leaves an orphaned server process behind — and
//! these are `npx` and `uvx` processes a user never started directly and has no
//! obvious way to find.
//!
//! # Standard error is discarded
//!
//! Servers write startup banners, progress, and warnings there, and none of it
//! is protocol. Standard *output* is the protocol channel, and a line on it
//! that is not JSON is skipped with a debug log rather than treated as a
//! failure, because servers print to it anyway.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::transport::{render_tool_result, validate_protocol_version};
use tinymcp_bus::{
    LATEST_PROTOCOL_VERSION, McpClientIdentityConfig, McpClientInfo, McpInitializeResult,
    McpRemoteTool, McpServerToolResult,
};

pub mod spawn_env;

/// An MCP client speaking JSON-RPC to a child process.
#[derive(Debug)]
pub struct McpStdioClient {
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
    next_id: AtomicI64,
    client_info: McpClientInfo,
    state: Mutex<Option<StdioSession>>,
}

/// A running child and the pipes to it.
#[derive(Debug)]
struct StdioSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    initialize: McpInitializeResult,
}

impl McpStdioClient {
    /// Builds a client that will spawn `command`.
    ///
    /// Nothing is spawned until [`Self::initialize`].
    #[must_use]
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cwd: Option<PathBuf>,
        identity: &McpClientIdentityConfig,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            env,
            cwd,
            next_id: AtomicI64::new(1),
            client_info: McpClientInfo::from(identity),
            state: Mutex::new(None),
        }
    }

    /// The command this client spawns.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Spawns the server and performs the handshake, or returns the cached
    /// result.
    ///
    /// The command is resolved against the reconstructed path *before* the
    /// spawn, so a missing runtime produces guidance naming it rather than a
    /// bare `ENOENT`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when the command cannot be found on
    /// the resolved path, when the child cannot be spawned, or when the reply
    /// is not the shape the protocol requires; and
    /// [`Error::UnsupportedProtocolVersion`] when the server settles on a
    /// version this client does not speak.
    pub async fn initialize(&self) -> Result<McpInitializeResult> {
        let mut state = self.state.lock().await;
        if let Some(session) = state.as_ref() {
            return Ok(session.initialize.clone());
        }

        let resolved_path = spawn_env::spawn_path().await;

        // A path in the server's own environment wins over the reconstructed
        // one, so resolution has to consider the same value the child will see.
        // The last entry wins, matching how the environment is applied below.
        let effective_path = self
            .env
            .iter()
            .rev()
            .find(|(key, _)| key == "PATH")
            .map_or(resolved_path.as_str(), |(_, value)| value.as_str());

        if spawn_env::locate_command(&self.command, effective_path, self.cwd.as_deref()).is_none() {
            tracing::warn!(
                command = %self.command,
                "the stdio command was not found on the resolved path"
            );
            return Err(Error::malformed(spawn_env::missing_command_error(
                &self.command,
            )));
        }

        let mut command = Command::new(&self.command);
        command
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // See the module note: a dropped client must not leave an orphaned
            // server process behind.
            .kill_on_drop(true);
        if let Some(cwd) = self.cwd.as_ref() {
            command.current_dir(cwd);
        }
        // The reconstructed path goes on first so an explicit `PATH` in the
        // server's own environment overrides it.
        command.env("PATH", &resolved_path);
        for (key, value) in &self.env {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|error| {
            Error::malformed(format!("spawning `{}` failed: {error}", self.command))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::malformed("the spawned server has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::malformed("the spawned server has no stdout"))?;

        let mut session = StdioSession {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            initialize: McpInitializeResult {
                protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
                capabilities: json!({}),
                server_info: json!({}),
                instructions: None,
            },
        };

        let response = self
            .request(
                &mut session,
                "initialize",
                json!({
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": self.client_info,
                }),
            )
            .await?;

        let initialized: McpInitializeResult = serde_json::from_value(response)
            .map_err(|error| Error::malformed(format!("stdio initialize result: {error}")))?;
        // The HTTP transport has always validated this; the stdio one did not,
        // and a subprocess is no more trustworthy than a remote endpoint.
        validate_protocol_version(&initialized.protocol_version)?;

        self.notify(&mut session, "notifications/initialized", json!({}))
            .await?;

        session.initialize = initialized.clone();
        *state = Some(session);
        Ok(initialized)
    }

    /// Lists the tools the server advertises.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when the reply has no `tools`
    /// member, plus anything [`Self::initialize`] can return.
    pub async fn list_tools(&self) -> Result<Vec<McpRemoteTool>> {
        self.initialize().await?;

        let mut state = self.state.lock().await;
        let session = state
            .as_mut()
            .ok_or_else(|| Error::malformed("the stdio session is not initialized"))?;

        let response = self.request(session, "tools/list", json!({})).await?;
        let tools = response
            .get("tools")
            .ok_or_else(|| Error::malformed("stdio tools/list reply has no `tools` member"))?;

        serde_json::from_value(tools.clone())
            .map_err(|error| Error::malformed(format!("stdio tools/list entries: {error}")))
    }

    /// Calls `name` with `arguments`.
    ///
    /// A tool that reports failure comes back as a result flagged an error, not
    /// as an `Err`; see [`crate::render_tool_result`].
    ///
    /// # Errors
    ///
    /// Returns whatever the transport returns, plus anything
    /// [`Self::initialize`] can return.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpServerToolResult> {
        self.initialize().await?;

        let mut state = self.state.lock().await;
        let session = state
            .as_mut()
            .ok_or_else(|| Error::malformed("the stdio session is not initialized"))?;

        let result = self
            .request(
                session,
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;

        let rendered = render_tool_result(&result);
        Ok(McpServerToolResult::new(result, rendered))
    }

    /// Terminates the child and forgets the session.
    ///
    /// Both the kill and the wait are best-effort: a child that has already
    /// exited is the outcome this method wants, so failing to signal one is not
    /// an error worth propagating.
    ///
    /// # Errors
    ///
    /// Never returns an error today. The signature is fallible so a future
    /// graceful-shutdown handshake does not become a breaking change.
    pub async fn close_session(&self) -> Result<()> {
        if let Some(mut session) = self.state.lock().await.take() {
            let _ = session.child.start_kill();
            let _ = session.child.wait().await;
        }
        Ok(())
    }

    /// Sends a request and reads until its reply arrives.
    ///
    /// Blank lines and lines that are not JSON are skipped: servers print to
    /// standard output whether or not they should, and treating a banner as a
    /// protocol violation would break servers that otherwise work.
    async fn request(
        &self,
        session: &mut StdioSession,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.write_line(
            session,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )
        .await?;

        loop {
            let mut line = String::new();
            let read = session.stdout.read_line(&mut line).await.map_err(|error| {
                Error::malformed(format!("reading from `{}` failed: {error}", self.command))
            })?;

            if read == 0 {
                return Err(Error::malformed(format!(
                    "`{}` closed its output while waiting for `{method}`",
                    self.command
                )));
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
                tracing::debug!(
                    command = %self.command,
                    "ignoring a non-json line on the server's output"
                );
                continue;
            }

            let payload: Value = serde_json::from_str(trimmed).map_err(|error| {
                Error::malformed(format!("stdio reply is not json: {error} — {trimmed}"))
            })?;

            if let Some(error) = payload.get("error") {
                return Err(Error::Rpc {
                    message: error.to_string(),
                });
            }

            return payload.get("result").cloned().ok_or_else(|| {
                Error::malformed(format!("stdio reply has no `result`: {payload}"))
            });
        }
    }

    /// Sends a notification, which carries no reply.
    async fn notify(&self, session: &mut StdioSession, method: &str, params: Value) -> Result<()> {
        self.write_line(
            session,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
        .await
    }

    /// Writes one newline-terminated JSON message and flushes it.
    async fn write_line(&self, session: &mut StdioSession, message: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');

        session.stdin.write_all(&line).await.map_err(|error| {
            Error::malformed(format!("writing to `{}` failed: {error}", self.command))
        })?;
        session.stdin.flush().await.map_err(|error| {
            Error::malformed(format!("flushing to `{}` failed: {error}", self.command))
        })
    }
}

#[cfg(test)]
mod test;
