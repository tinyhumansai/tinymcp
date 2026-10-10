//! The object-safe host callback seam over shared contract DTOs.

use futures_util::future::BoxFuture;
use serde_json::{Map, Value, json};
pub use tinymcp_bus::server::{
    DEFAULT_SOURCE_TYPE_PREFIX, RequestContext, RequestHeaders, ResourceSpec, ServerInfo,
    ServerToolSpec, ToolCallError,
};
pub(crate) use tinymcp_bus::server::{
    INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
};

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
