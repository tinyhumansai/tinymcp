//! A small, fully deterministic handler the server suites run against.
//!
//! It exists so the protocol and transport tests exercise real dispatch
//! through a real [`McpServerHandler`] rather than a mock of one, and so each
//! behavior they pin — a tool that fails as invalid input, one that fails
//! internally, one that runs and reports `isError` — has a tool that does
//! exactly that.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::RequestContextExt;
use futures_util::future::BoxFuture;
use serde_json::{Map, Value, json};

use super::{
    McpServerHandler, RequestContext, ResourceSpec, ServerInfo, ServerToolSpec, ToolCallError,
};

/// The header the `echo` tool reports back, to prove headers reach handlers.
pub(crate) const ECHOED_HEADER: &str = "X-Demo-Depth";

/// The demo handler. `instructions: false` serves an identity without them.
pub(crate) struct DemoHandler {
    pub(crate) instructions: bool,
}

impl DemoHandler {
    pub(crate) const fn new() -> Self {
        Self { instructions: true }
    }
}

impl McpServerHandler for DemoHandler {
    fn server_info(&self) -> ServerInfo {
        let info = ServerInfo::new("demo-server", "9.9.9");
        if self.instructions {
            info.with_instructions("Use the demo tools.")
        } else {
            info
        }
    }

    fn source_type_prefix(&self) -> &'static str {
        "demo"
    }

    fn list_tools<'a>(&'a self, _ctx: &'a RequestContext) -> BoxFuture<'a, Vec<ServerToolSpec>> {
        Box::pin(async {
            vec![
                ServerToolSpec::new("echo", "Echo the arguments.", json!({"type": "object"}))
                    .with_title("Echo")
                    .with_annotations(json!({"readOnlyHint": true})),
                ServerToolSpec::new("bare", "No title.", json!({})),
            ]
        })
    }

    fn call_tool<'a>(
        &'a self,
        ctx: &'a RequestContext,
        name: &'a str,
        arguments: Map<String, Value>,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async move {
            match name {
                "echo" => Ok(json!({
                    "content": [{"type": "text", "text": Value::Object(arguments).to_string()}],
                    "structuredContent": {
                        "source_type": ctx.source_type(),
                        "depth": ctx.header(ECHOED_HEADER),
                    },
                })),
                "soft_error" => Ok(json!({
                    "content": [{"type": "text", "text": "it ran and failed"}],
                    "isError": true,
                })),
                "fail_invalid" => Err(ToolCallError::InvalidParams("bad input".to_string())),
                "fail_internal" => Err(ToolCallError::Internal("boom".to_string())),
                other => Err(ToolCallError::InvalidParams(format!(
                    "unknown MCP tool `{other}`"
                ))),
            }
        })
    }

    fn list_resources(&self) -> Vec<ResourceSpec> {
        vec![
            ResourceSpec::new("demo://readme", "Readme")
                .with_description("The readme.")
                .with_mime_type("text/markdown"),
        ]
    }

    fn read_resource<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async move {
            if uri == "demo://readme" {
                Ok(
                    json!({"contents": [{"uri": uri, "mimeType": "text/markdown", "text": "# Demo"}]}),
                )
            } else {
                Err(ToolCallError::ResourceNotFound(format!(
                    "no resource with uri `{uri}`"
                )))
            }
        })
    }
}
