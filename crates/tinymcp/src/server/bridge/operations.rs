//! Replayable operations, acquisition reservations and joined cancellation.

use super::handler::{CallbackHandler, WaitingCallback};
use crate::server::{ClientSession, RequestHeaders, handle_line};
use crate::{Error, Result};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tinymcp_bus::{
    ServerHostReply, ServerInput, ServerOperationSnapshot, ServerOperationState,
    ServerSessionConfig,
};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

struct Operation {
    input: ServerInput,
    headers: RequestHeaders,
    task: Option<JoinHandle<Option<String>>>,
    receiver: mpsc::Receiver<WaitingCallback>,
    waiting: Option<WaitingCallback>,
    terminal: Option<ServerOperationState>,
    reply_bytes: usize,
}
impl Drop for Operation {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Operation {
    async fn finish(&mut self) {
        if self.task.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(task) = self.task.take()
        {
            self.terminal = Some(match task.await {
                Ok(response)
                    if response
                        .as_ref()
                        .is_some_and(|line| line.len() > super::MAX_BYTES) =>
                {
                    ServerOperationState::Failed {
                        message: "server output exceeds byte limit".into(),
                    }
                }
                Ok(response) => ServerOperationState::Complete { response },
                Err(_) => ServerOperationState::Failed {
                    message: "server operation stopped".into(),
                },
            });
        }
    }
    async fn stop(&mut self) {
        self.waiting = None;
        self.receiver.close();
        while self.receiver.try_recv().is_ok() {}
        if let Some(task) = self.task.as_mut() {
            task.abort();
            let _ = task.await;
        }
        self.task = None;
    }
}
struct Session {
    client: Arc<Mutex<ClientSession>>,
    config: ServerSessionConfig,
    operation: Option<Operation>,
    used_operations: HashSet<String>,
}

/// One registry object's bounded server sessions and non-reusable reservations.
#[derive(Default)]
pub struct ServerSessions {
    sessions: Mutex<HashMap<String, Option<Session>>>,
}
impl std::fmt::Debug for ServerSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerSessions").finish_non_exhaustive()
    }
}
fn invalid(message: &str) -> Error {
    Error::invalid_argument(message)
}
fn validate_id(id: &str) -> Result<()> {
    uuid::Uuid::parse_str(id).map_err(|_| invalid("invalid server identifier"))?;
    Ok(())
}
fn bounded<T: serde::Serialize>(value: &T) -> Result<usize> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| invalid("invalid server payload"))?
        .len();
    if bytes > super::MAX_BYTES {
        return Err(invalid("server payload exceeds byte limit"));
    }
    Ok(bytes)
}
impl ServerSessions {
    /// Opens or retries a caller-known session reservation with frozen declarations.
    ///
    /// # Errors
    /// Rejects invalid declarations, conflicting IDs, more than 32 active sessions,
    /// oversized payloads or more than 4096 lifetime reservations per object.
    pub async fn open(&self, config: ServerSessionConfig) -> Result<String> {
        bounded(&config)?;
        let _: crate::server::ServerInfo = serde_json::from_value(config.info.clone())
            .map_err(|_| invalid("invalid server identity"))?;
        let _: Vec<crate::server::ResourceSpec> =
            serde_json::from_value(Value::Array(config.resources.clone()))
                .map_err(|_| invalid("invalid server resources"))?;
        if config.source_type_prefix.trim().is_empty() {
            return Err(invalid("empty server provenance prefix"));
        }
        validate_id(&config.session_id)?;
        let mut sessions = self.sessions.lock().await;
        if let Some(existing) = sessions.get(&config.session_id) {
            return match existing {
                Some(session) if session.config == config => Ok(config.session_id),
                _ => Err(invalid("server session identifier already reserved")),
            };
        }
        if sessions.len() >= 4096 {
            return Err(invalid("server reservation limit reached"));
        }
        if sessions
            .values()
            .filter(|session| session.is_some())
            .count()
            >= 32
        {
            return Err(invalid("server session limit reached"));
        }
        let id = config.session_id.clone();
        sessions.insert(
            id.clone(),
            Some(Session {
                client: Arc::new(Mutex::new(ClientSession::new(&config.source_type_prefix))),
                config,
                operation: None,
                used_operations: HashSet::new(),
            }),
        );
        Ok(id)
    }
    /// Submits or retries caller-known input. New input acknowledges prior terminal state.
    ///
    /// # Errors
    /// Rejects closed sessions, concurrent operations, conflicting/retired IDs,
    /// invalid headers, byte limits and over 4096 operations in one session.
    pub async fn submit(
        &self,
        id: &str,
        headers: Map<String, Value>,
        input: ServerInput,
    ) -> Result<()> {
        bounded(&input)?;
        bounded(&headers)?;
        validate_id(&input.operation_id)?;
        if serde_json::from_str::<Value>(&input.line)
            .ok()
            .is_some_and(|value| value.as_array().is_some_and(|items| items.len() > 256))
        {
            return Err(invalid("server batch exceeds item limit"));
        }
        let mut request_headers = RequestHeaders::new();
        for (key, value) in headers {
            request_headers.insert(
                &key,
                value
                    .as_str()
                    .ok_or_else(|| invalid("server header values must be strings"))?,
            );
        }
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or_else(|| invalid("unknown server session"))?;
        if let Some(operation) = &session.operation {
            if operation.input.operation_id == input.operation_id {
                return if operation.input == input && operation.headers == request_headers {
                    Ok(())
                } else {
                    Err(invalid("conflicting server operation retry"))
                };
            }
            if operation.terminal.is_none() {
                return Err(invalid("server session already has an operation"));
            }
        }
        if session.used_operations.contains(&input.operation_id) {
            return Err(invalid("server operation identifier already used"));
        }
        if session.used_operations.len() >= 4096 {
            return Err(invalid("server operation limit reached"));
        }
        let (sender, receiver) = mpsc::channel(1);
        let handler = Arc::new(CallbackHandler {
            info: serde_json::from_value(session.config.info.clone())
                .map_err(|_| invalid("invalid server identity"))?,
            prefix: session.config.source_type_prefix.clone(),
            resources: serde_json::from_value(Value::Array(session.config.resources.clone()))
                .map_err(|_| invalid("invalid server resources"))?,
            sender,
        });
        let client = Arc::clone(&session.client);
        let line = input.line.clone();
        let task_headers = request_headers.clone();
        let task = tokio::spawn(async move {
            let mut client = client.lock().await;
            handle_line(&handler, &mut client, &task_headers, &line).await
        });
        session.used_operations.insert(input.operation_id.clone());
        session.operation = Some(Operation {
            input,
            headers: request_headers,
            task: Some(task),
            receiver,
            waiting: None,
            terminal: None,
            reply_bytes: 0,
        });
        Ok(())
    }
    /// Observes replayable callbacks and terminal states without destructive drains.
    ///
    /// # Errors
    /// Rejects unknown or idle sessions.
    pub async fn poll(&self, id: &str) -> Result<ServerOperationSnapshot> {
        let mut sessions = self.sessions.lock().await;
        let operation = sessions
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or_else(|| invalid("unknown server session"))?
            .operation
            .as_mut()
            .ok_or_else(|| invalid("server session has no operation"))?;
        operation.finish().await;
        let state = if let Some(terminal) = &operation.terminal {
            terminal.clone()
        } else if let Some(waiting) = &operation.waiting {
            ServerOperationState::Callback {
                callback: waiting.callback.clone(),
            }
        } else if let Ok(waiting) = operation.receiver.try_recv() {
            let callback = waiting.callback.clone();
            operation.waiting = Some(waiting);
            ServerOperationState::Callback { callback }
        } else {
            ServerOperationState::Pending
        };
        Ok(ServerOperationSnapshot {
            operation_id: operation.input.operation_id.clone(),
            state,
        })
    }
    /// Completes one outstanding callback after host policy and execution.
    ///
    /// # Errors
    /// Rejects closed sessions, stale callback IDs and excessive aggregate replies.
    pub async fn complete(
        &self,
        id: &str,
        callback_id: &str,
        reply: ServerHostReply,
    ) -> Result<()> {
        let bytes = bounded(&reply)?;
        let mut sessions = self.sessions.lock().await;
        let operation = sessions
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or_else(|| invalid("unknown server session"))?
            .operation
            .as_mut()
            .ok_or_else(|| invalid("server session has no operation"))?;
        if operation.terminal.is_some() {
            return Err(invalid("server operation finished"));
        }
        let waiting = operation
            .waiting
            .as_ref()
            .ok_or_else(|| invalid("server callback is not outstanding"))?;
        if waiting.callback.id != callback_id {
            return Err(invalid("unknown server callback"));
        }
        if operation.reply_bytes + bytes > super::MAX_BYTES {
            return Err(invalid("server replies exceed aggregate byte limit"));
        }
        operation.reply_bytes += bytes;
        let waiting = operation
            .waiting
            .take()
            .ok_or_else(|| invalid("server callback is not outstanding"))?;
        waiting
            .reply
            .send(reply)
            .map_err(|_| invalid("server callback stopped"))
    }
    /// Cancels and joins active protocol work. Already terminal operations are preserved.
    ///
    /// # Errors
    /// Rejects unknown or idle sessions.
    pub async fn cancel(&self, id: &str, operation_id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let operation = sessions
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or_else(|| invalid("unknown server session"))?
            .operation
            .as_mut()
            .ok_or_else(|| invalid("server session has no operation"))?;
        if operation.input.operation_id != operation_id {
            return Err(invalid("stale server cancellation"));
        }
        operation.finish().await;
        if operation.terminal.is_none() {
            operation.stop().await;
            operation.terminal = Some(ServerOperationState::Cancelled);
        }
        Ok(())
    }
    /// Closes a caller-known reservation, including one whose open is still queued.
    ///
    /// # Errors
    /// Rejects malformed identifiers or the lifetime reservation limit.
    pub async fn close(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        let mut sessions = self.sessions.lock().await;
        if !sessions.contains_key(id) && sessions.len() >= 4096 {
            return Err(invalid("server reservation limit reached"));
        }
        if let Some(mut session) = sessions.insert(id.into(), None).flatten()
            && let Some(mut operation) = session.operation.take()
        {
            operation.stop().await;
        }
        Ok(())
    }
    /// Closes and joins all sessions on this object, retaining closed reservations.
    pub async fn shutdown(&self) -> usize {
        let mut sessions = self.sessions.lock().await;
        let mut count = 0;
        for entry in sessions.values_mut() {
            if let Some(mut session) = entry.take() {
                count += 1;
                if let Some(mut operation) = session.operation.take() {
                    operation.stop().await;
                }
            }
        }
        count
    }
}

#[cfg(test)]
#[path = "operations_tests.rs"]
mod tests;
