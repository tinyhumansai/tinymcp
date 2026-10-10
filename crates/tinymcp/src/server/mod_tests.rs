//! Unit tests for the server seam: the handler error, the data a handler
//! provides, and the request context it receives.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Map, Value, json};

use super::{
    McpServerHandler, RequestContext, RequestHeaders, ResourceSpec, ServerInfo, ServerToolSpec,
    ToolCallError,
};
use futures_util::future::BoxFuture;

// ---------------------------------------------------------------------------
// ToolCallError
// ---------------------------------------------------------------------------

#[test]
fn invalid_params_maps_to_jsonrpc_invalid_params() {
    let err = ToolCallError::InvalidParams("missing query".to_string());
    assert_eq!(err.code(), -32602);
    assert_eq!(err.jsonrpc_message(), "Invalid params");
    assert_eq!(err.message(), "missing query");
    assert_eq!(err.to_string(), "missing query");
}

#[test]
fn internal_maps_to_jsonrpc_internal_error() {
    // Server-side failures must not read as bad arguments, or a client
    // retries with different ones.
    let err = ToolCallError::Internal("disk read failed".to_string());
    assert_eq!(err.code(), -32603);
    assert_eq!(err.jsonrpc_message(), "Internal error");
    assert_eq!(err.message(), "disk read failed");
}

#[test]
fn resource_not_found_maps_to_the_mcp_resource_code() {
    let err = ToolCallError::ResourceNotFound("no resource with uri `x`".to_string());
    assert_eq!(err.code(), -32002);
    assert_eq!(err.jsonrpc_message(), "Resource not found");
    assert_eq!(err.message(), "no resource with uri `x`");
}

// ---------------------------------------------------------------------------
// Handler-provided data
// ---------------------------------------------------------------------------

#[test]
fn server_info_carries_optional_instructions() {
    let bare = ServerInfo::new("demo", "1.2.3");
    assert_eq!(bare.name, "demo");
    assert_eq!(bare.version, "1.2.3");
    assert_eq!(bare.instructions, None);
    let guided = bare.with_instructions("use the tools");
    assert_eq!(guided.instructions.as_deref(), Some("use the tools"));
}

#[test]
fn a_full_tool_spec_renders_every_field() {
    let spec = ServerToolSpec::new("memory.search", "Search memory.", json!({"type": "object"}))
        .with_title("Search Memory")
        .with_annotations(json!({"readOnlyHint": true}));
    assert_eq!(
        spec.to_json(),
        json!({
            "name": "memory.search",
            "title": "Search Memory",
            "description": "Search memory.",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true},
        })
    );
}

#[test]
fn a_bare_tool_spec_omits_title_and_annotations() {
    let spec = ServerToolSpec::new("ping", "Ping.", json!({}));
    assert_eq!(
        spec.to_json(),
        json!({"name": "ping", "description": "Ping.", "inputSchema": {}})
    );
}

#[test]
fn a_full_resource_spec_renders_every_field() {
    let spec = ResourceSpec::new("demo://a", "A")
        .with_description("The a resource.")
        .with_mime_type("text/markdown");
    assert_eq!(
        spec.to_json(),
        json!({
            "uri": "demo://a",
            "name": "A",
            "description": "The a resource.",
            "mimeType": "text/markdown",
        })
    );
}

#[test]
fn a_bare_resource_spec_omits_description_and_mime_type() {
    let spec = ResourceSpec::new("demo://a", "A");
    assert_eq!(spec.to_json(), json!({"uri": "demo://a", "name": "A"}));
}

// ---------------------------------------------------------------------------
// Request context
// ---------------------------------------------------------------------------

#[test]
fn headers_are_looked_up_case_insensitively() {
    let mut headers = RequestHeaders::new();
    headers.insert("X-Depth", "2");
    assert_eq!(headers.get("x-depth"), Some("2"));
    assert_eq!(headers.get("X-DEPTH"), Some("2"));
    assert_eq!(headers.get("missing"), None);
}

#[test]
fn the_first_value_of_a_repeated_header_wins() {
    let mut headers = RequestHeaders::new();
    headers.insert("x-depth", "1");
    headers.insert("X-Depth", "5");
    assert_eq!(headers.get("x-depth"), Some("1"));
}

#[test]
fn header_debug_output_names_headers_without_their_values() {
    // A bearer token rides in `authorization`; a context that reaches a log
    // line must not carry it there.
    let mut headers = RequestHeaders::new();
    headers.insert("Authorization", "Bearer secret-token");
    let context = RequestContext::new("mcp", headers);
    let rendered = format!("{context:?}");
    assert!(rendered.contains("authorization"), "{rendered}");
    assert!(!rendered.contains("secret-token"), "{rendered}");
}

#[test]
fn a_context_exposes_its_source_type_and_headers() {
    let mut headers = RequestHeaders::new();
    headers.insert("x-a", "1");
    let context = RequestContext::new("mcp:cursor", headers);
    assert_eq!(context.source_type(), "mcp:cursor");
    assert_eq!(context.header("X-A"), Some("1"));
    assert_eq!(context.headers().get("x-a"), Some("1"));
}

// ---------------------------------------------------------------------------
// Handler defaults
// ---------------------------------------------------------------------------

struct Minimal;

impl McpServerHandler for Minimal {
    fn server_info(&self) -> ServerInfo {
        ServerInfo::new("minimal", "0")
    }

    fn list_tools<'a>(&'a self, _ctx: &'a RequestContext) -> BoxFuture<'a, Vec<ServerToolSpec>> {
        Box::pin(async { Vec::new() })
    }

    fn call_tool<'a>(
        &'a self,
        _ctx: &'a RequestContext,
        name: &'a str,
        _arguments: Map<String, Value>,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async move { Err(ToolCallError::InvalidParams(format!("no tool `{name}`"))) })
    }
}

#[tokio::test]
async fn a_handler_without_resources_lists_none_and_reads_none() {
    let handler = Minimal;
    assert_eq!(handler.source_type_prefix(), "mcp");
    assert_eq!(handler.list_resources().len(), 0);
    let err = handler
        .read_resource("demo://a")
        .await
        .expect_err("no resources");
    assert_eq!(
        err,
        ToolCallError::ResourceNotFound("no resource with uri `demo://a`".to_string())
    );
    let context = RequestContext::new("mcp", RequestHeaders::new());
    assert_eq!(handler.list_tools(&context).await.len(), 0);
    let err = handler
        .call_tool(&context, "x", Map::new())
        .await
        .expect_err("no tools");
    assert_eq!(err.message(), "no tool `x`");
}

#[tokio::test]
async fn prompt_defaults_preserve_existing_handlers_and_reject_unknown_prompts() {
    let handler = Minimal;
    let ctx = RequestContext::new("mcp", RequestHeaders::new());
    assert!(!handler.supports_prompts());
    assert_eq!(
        handler.list_prompts(&ctx).await.unwrap(),
        json!({"prompts":[]})
    );
    assert_eq!(
        handler
            .get_prompt(&ctx, "missing", Map::new())
            .await
            .unwrap_err(),
        ToolCallError::InvalidParams("unknown prompt `missing`".into())
    );
}
