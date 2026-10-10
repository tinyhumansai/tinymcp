//! The server seam: what a host implements, and the data crossing it.

use std::collections::BTreeMap;
use std::fmt;

use futures_util::future::BoxFuture;
use serde_json::{Map, Value, json};

/// JSON-RPC `Parse error`.
pub(crate) const PARSE_ERROR: i64 = -32700;
/// JSON-RPC `Invalid Request`.
pub(crate) const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC `Method not found`.
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC `Invalid params`.
pub(crate) const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC `Internal error`.
pub(crate) const INTERNAL_ERROR: i64 = -32603;
/// MCP's `Resource not found`, from the implementation-defined server range.
pub(crate) const RESOURCE_NOT_FOUND: i64 = -32002;

/// The source type a session reports before, or without, a usable
/// `clientInfo.name`.
pub const DEFAULT_SOURCE_TYPE_PREFIX: &str = "mcp";

/// Why a handler refused a tool call or a resource read.
///
/// Each variant maps to one JSON-RPC error code, and the split is the point:
/// a client shows `InvalidParams` as something the caller can fix and
/// `Internal` as something it cannot, so a server-side failure reported as bad
/// arguments sends a model off retrying with different ones.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolCallError {
    /// A problem with the call: unknown tool, malformed or missing arguments,
    /// a policy denial the caller can act on. `-32602 Invalid params`.
    #[error("{0}")]
    InvalidParams(String),
    /// A problem on the server's side: configuration that would not load, a
    /// platform resource that is missing. `-32603 Internal error`.
    #[error("{0}")]
    Internal(String),
    /// `resources/read` named a URI the server does not serve.
    /// `-32002 Resource not found`.
    #[error("{0}")]
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
    pub title: Option<String>,
    /// What the tool does, for the model choosing it.
    pub description: String,
    /// The JSON Schema its `arguments` must satisfy.
    pub input_schema: Value,
    /// MCP tool annotations (`readOnlyHint`, `destructiveHint`, …). Omitted
    /// when `None`.
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

    /// The entry as it appears in a `tools/list` result.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut entry = Map::new();
        entry.insert("name".to_string(), json!(self.name));
        if let Some(title) = &self.title {
            entry.insert("title".to_string(), json!(title));
        }
        entry.insert("description".to_string(), json!(self.description));
        entry.insert("inputSchema".to_string(), self.input_schema.clone());
        if let Some(annotations) = &self.annotations {
            entry.insert("annotations".to_string(), annotations.clone());
        }
        Value::Object(entry)
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
    pub description: Option<String>,
    /// Its media type. Omitted when `None`.
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

    /// The entry as it appears in a `resources/list` result.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut entry = Map::new();
        entry.insert("uri".to_string(), json!(self.uri));
        entry.insert("name".to_string(), json!(self.name));
        if let Some(description) = &self.description {
            entry.insert("description".to_string(), json!(description));
        }
        if let Some(mime_type) = &self.mime_type {
            entry.insert("mimeType".to_string(), json!(mime_type));
        }
        Value::Object(entry)
    }
}

/// The transport headers a request arrived with.
///
/// Names are case-insensitive, as HTTP's are, and the first value of a
/// repeated header wins. Stdio requests carry none.
///
/// `Debug` prints header *names* only: `authorization` carries a bearer token,
/// and a context is exactly the kind of value that ends up in a log line.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RequestHeaders {
    pub(super) entries: BTreeMap<String, String>,
}

impl RequestHeaders {
    /// No headers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a header, unless one of that name is already present.
    pub fn insert(&mut self, name: &str, value: impl Into<String>) {
        self.entries
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| value.into());
    }

    /// The value of the named header, if the request carried it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

impl fmt::Debug for RequestHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.entries.keys()).finish()
    }
}

/// What a handler knows about the request it is answering.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// [`crate::server::ClientSession`].
    #[must_use]
    pub fn source_type(&self) -> &str {
        &self.source_type
    }

    /// The value of the named transport header, if the request carried it.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    /// Every transport header the request carried.
    #[must_use]
    pub fn headers(&self) -> &RequestHeaders {
        &self.headers
    }
}

/// What a host implements to serve MCP.
///
/// The protocol — framing, batching, notifications, version negotiation,
/// method routing and every error shape — belongs to this crate. The handler
/// supplies only what is the host's to decide: who the server is, which tools
/// and resources it offers, and what calling one does.
///
/// Methods returning a future return it boxed, so the trait stays
/// object-safe and a transport can hold an `Arc<dyn McpServerHandler>`.
pub trait McpServerHandler: Send + Sync {
    /// The identity and instructions the `initialize` result carries.
    fn server_info(&self) -> ServerInfo;

    /// The base of every session's source type: a session reports this alone,
    /// or `<prefix>:<client-name>` once `initialize` names the client.
    fn source_type_prefix(&self) -> &str {
        DEFAULT_SOURCE_TYPE_PREFIX
    }

    /// The tools `tools/list` advertises, in order.
    fn list_tools<'a>(&'a self, ctx: &'a RequestContext) -> BoxFuture<'a, Vec<ServerToolSpec>>;

    /// Lists tools while preserving a host callback failure as a protocol error.
    fn list_tools_result<'a>(
        &'a self,
        ctx: &'a RequestContext,
    ) -> BoxFuture<'a, Result<Vec<ServerToolSpec>, ToolCallError>> {
        Box::pin(async move { Ok(self.list_tools(ctx).await) })
    }

    /// Runs one tool. `arguments` has already been read as an object: absent
    /// arguments arrive empty, and a JSON-encoded object arrives decoded.
    ///
    /// `Ok` is the `tools/call` result — including a result carrying
    /// `isError: true`, which is a tool that ran and failed. `Err` is a call
    /// that could not run at all, and becomes a JSON-RPC error.
    fn call_tool<'a>(
        &'a self,
        ctx: &'a RequestContext,
        name: &'a str,
        arguments: Map<String, Value>,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>>;

    /// Whether this handler advertises host-owned prompts.
    fn supports_prompts(&self) -> bool {
        false
    }

    /// The prompts this client may discover. Empty by default.
    fn list_prompts<'a>(
        &'a self,
        _ctx: &'a RequestContext,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async { Ok(json!({ "prompts": [] })) })
    }

    /// Resolves a prompt using host-owned policy and domain dispatch.
    fn get_prompt<'a>(
        &'a self,
        _ctx: &'a RequestContext,
        name: &'a str,
        _arguments: Map<String, Value>,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async move {
            Err(ToolCallError::InvalidParams(format!(
                "unknown prompt `{name}`"
            )))
        })
    }

    /// Reads a resource with the caller's provenance and transport headers.
    fn read_resource_context<'a>(
        &'a self,
        _ctx: &'a RequestContext,
        uri: &'a str,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        self.read_resource(uri)
    }

    /// The resources `resources/list` advertises. None by default.
    fn list_resources(&self) -> Vec<ResourceSpec> {
        Vec::new()
    }

    /// The `resources/read` result for `uri`. By default every URI is
    /// [`ToolCallError::ResourceNotFound`].
    fn read_resource<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async move {
            Err(ToolCallError::ResourceNotFound(format!(
                "no resource with uri `{uri}`"
            )))
        })
    }
}
