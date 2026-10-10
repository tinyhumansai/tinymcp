//! Serialized vocabulary for module-owned server operations.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Host-owned declarations frozen for one server session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerSessionConfig {
    /// Caller-generated UUID, known before acquisition so lost replies remain cancellable.
    pub session_id: String,
    /// Server identity, version and optional instructions.
    pub info: Value,
    /// Host-selected provenance prefix.
    pub source_type_prefix: String,
    /// MCP resource declarations served by resources/list.
    pub resources: Vec<Value>,
}

/// A validated operation awaiting host policy and execution.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerHostCall {
    /// List the tools the host permits this client to see.
    ListTools {
        /// Module-derived client provenance.
        source_type: String,
        /// Transport headers supplied with this operation.
        headers: Map<String, Value>,
    },
    /// Apply approvals and dispatch one tool.
    CallTool {
        /// Module-derived client provenance.
        source_type: String,
        /// Transport headers supplied with this operation.
        headers: Map<String, Value>,
        /// Validated tool name.
        name: String,
        /// Normalized argument object.
        arguments: Map<String, Value>,
    },
    /// Read a declared resource through the host's domain dispatcher.
    ReadResource {
        /// Module-derived client provenance.
        source_type: String,
        /// Transport headers supplied with this operation.
        headers: Map<String, Value>,
        /// Resource URI validated by the protocol.
        uri: String,
    },
    /// List host-owned prompt declarations.
    ListPrompts {
        /// Module-derived client provenance.
        source_type: String,
        /// Transport headers supplied with this operation.
        headers: Map<String, Value>,
    },
    /// Resolve a named host-owned prompt.
    GetPrompt {
        /// Module-derived client provenance.
        source_type: String,
        /// Transport headers supplied with this operation.
        headers: Map<String, Value>,
        /// Validated prompt name.
        name: String,
        /// String-valued MCP prompt arguments.
        arguments: Map<String, Value>,
    },
}

/// A host's result after applying its own policy and domain operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerHostReply {
    /// A successful MCP result, or an array of MCP tool declarations for list.
    Success {
        /// Result payload.
        value: Value,
    },
    /// A caller-correctable error.
    InvalidParams {
        /// Human-readable detail.
        message: String,
    },
    /// A host-side failure.
    Internal {
        /// Human-readable detail.
        message: String,
    },
    /// A URI unavailable in the host's resource domain.
    ResourceNotFound {
        /// Human-readable detail.
        message: String,
    },
}

/// One callback emitted by a pending server operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerCallback {
    /// Opaque, single-use callback identifier.
    pub id: String,
    /// Validated domain operation.
    pub call: ServerHostCall,
}

/// The current observable state of a submitted input line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServerOperationState {
    /// Processing input, or waiting for a previously drained callback reply.
    Pending,
    /// Apply host policy and reply through `ServerComplete`.
    Callback {
        /// Single outstanding host callback.
        callback: ServerCallback,
    },
    /// Finished; remains replayable until the next operation acknowledges it.
    Complete {
        /// Framed JSON-RPC response, absent for notifications.
        response: Option<String>,
    },
    /// Cancelled; remains replayable until the next operation acknowledges it.
    Cancelled,
    /// A module operation could not produce a bounded protocol response.
    Failed {
        /// Stable module-owned failure description.
        message: String,
    },
}

impl std::fmt::Debug for ServerHostCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::ListTools { .. } => "ListTools",
            Self::CallTool { .. } => "CallTool",
            Self::ReadResource { .. } => "ReadResource",
            Self::ListPrompts { .. } => "ListPrompts",
            Self::GetPrompt { .. } => "GetPrompt",
        };
        f.debug_struct(kind).finish_non_exhaustive()
    }
}

/// Input identity known before submission, allowing side-effect-safe retries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerInput {
    /// Caller-generated UUID, never reused within this session.
    pub operation_id: String,
    /// One newline-delimited MCP message or batch.
    pub line: String,
}

/// Replayable observation of one caller-known operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerOperationSnapshot {
    /// Identifies the operation even when a poll reply arrives late.
    pub operation_id: String,
    /// Current callback or terminal state.
    pub state: ServerOperationState,
}

/// Cancellation targets the operation known before submission, never its successor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerOperationRef {
    /// Caller-known session UUID.
    pub session_id: String,
    /// Caller-known operation UUID.
    pub operation_id: String,
}
