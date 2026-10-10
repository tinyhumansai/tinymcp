//! Shared server declarations, request provenance and error vocabulary.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

/// JSON-RPC `Parse error`.
pub const PARSE_ERROR: i64 = -32700;
/// JSON-RPC `Invalid Request`.
pub const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC `Method not found`.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC `Invalid params`.
pub const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC `Internal error`.
pub const INTERNAL_ERROR: i64 = -32603;
/// MCP's `Resource not found`, from the implementation-defined server range.
pub const RESOURCE_NOT_FOUND: i64 = -32002;

/// The source type a session reports before, or without, a usable
/// `clientInfo.name`.
pub const DEFAULT_SOURCE_TYPE_PREFIX: &str = "mcp";

/// Why a handler refused a tool call or a resource read.
///
/// Each variant maps to one JSON-RPC error code, and the split is the point:
/// a client shows `InvalidParams` as something the caller can fix and
/// `Internal` as something it cannot, so a server-side failure reported as bad
/// arguments sends a model off retrying with different ones.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ToolCallError {
    /// A problem with the call: unknown tool, malformed or missing arguments,
    /// a policy denial the caller can act on. `-32602 Invalid params`.
    InvalidParams(String),
    /// A problem on the server's side: configuration that would not load, a
    /// platform resource that is missing. `-32603 Internal error`.
    Internal(String),
    /// `resources/read` named a URI the server does not serve.
    /// `-32002 Resource not found`.
    ResourceNotFound(String),
}

impl ToolCallError {
    /// The human-readable detail, sent as the JSON-RPC error's `data`.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidParams(message)
            | Self::Internal(message)
            | Self::ResourceNotFound(message) => message,
        }
    }

    /// The JSON-RPC error code for this variant.
    #[must_use]
    pub const fn code(&self) -> i64 {
        match self {
            Self::InvalidParams(_) => INVALID_PARAMS,
            Self::Internal(_) => INTERNAL_ERROR,
            Self::ResourceNotFound(_) => RESOURCE_NOT_FOUND,
        }
    }

    /// The JSON-RPC error's `message`: the short, spec-canonical phrase. The
    /// detail belongs in `data`; see [`Self::message`].
    #[must_use]
    pub const fn jsonrpc_message(&self) -> &'static str {
        match self {
            Self::InvalidParams(_) => "Invalid params",
            Self::Internal(_) => "Internal error",
            Self::ResourceNotFound(_) => "Resource not found",
        }
    }
}

/// Who this server says it is in the `initialize` result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServerInfo {
    /// `serverInfo.name`.
    pub name: String,
    /// `serverInfo.version`.
    pub version: String,
    /// Top-level `instructions`: guidance a client may show its model. Omitted
    /// from the result when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl ServerInfo {
    /// A server identity with no instructions.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            instructions: None,
        }
    }

    /// Sets the `instructions` the `initialize` result carries.
    #[must_use]
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }
}

/// One tool as `tools/list` advertises it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerToolSpec {
    /// The name a client calls it by.
    pub name: String,
    /// A display title. Omitted when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the tool does, for the model choosing it.
    pub description: String,
    /// The JSON Schema its `arguments` must satisfy.
    pub input_schema: Value,
    /// MCP tool annotations (`readOnlyHint`, `destructiveHint`, …). Omitted
    /// when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
}

impl ServerToolSpec {
    /// A tool with no title and no annotations.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            title: None,
            description: description.into(),
            input_schema,
            annotations: None,
        }
    }

    /// Sets the display title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets the tool annotations.
    #[must_use]
    pub fn with_annotations(mut self, annotations: Value) -> Self {
        self.annotations = Some(annotations);
        self
    }
}

/// One resource as `resources/list` advertises it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpec {
    /// The URI a client reads it by.
    pub uri: String,
    /// A display name.
    pub name: String,
    /// What it holds. Omitted when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Its media type. Omitted when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

impl ResourceSpec {
    /// A resource with no description and no media type.
    #[must_use]
    pub fn new(uri: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            name: name.into(),
            description: None,
            mime_type: None,
        }
    }

    /// Sets the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the media type.
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }
}

/// The transport headers a request arrived with.
///
/// Names are case-insensitive, as HTTP's are, and the first value of a
/// repeated header wins. Stdio requests carry none.
///
/// `Debug` prints header *names* only: `authorization` carries a bearer token,
/// and a context is exactly the kind of value that ends up in a log line.
#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RequestHeaders {
    /// Transport entries; module insertion normalizes names and retains the first value.
    pub entries: BTreeMap<String, String>,
}

impl RequestHeaders {
    /// No headers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl fmt::Debug for RequestHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.entries.keys()).finish()
    }
}

/// What a handler knows about the request it is answering.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestContext {
    source_type: String,
    headers: RequestHeaders,
}

impl RequestContext {
    /// A context for a request from `source_type`, carrying `headers`.
    #[must_use]
    pub fn new(source_type: impl Into<String>, headers: RequestHeaders) -> Self {
        Self {
            source_type: source_type.into(),
            headers,
        }
    }

    /// The calling client's provenance, e.g. `mcp:claude-desktop`. See
    /// the module-owned client session.
    #[must_use]
    pub fn source_type(&self) -> &str {
        &self.source_type
    }

    /// Every transport header the request carried.
    #[must_use]
    pub fn headers(&self) -> &RequestHeaders {
        &self.headers
    }
}

impl fmt::Display for ToolCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for ToolCallError {}
