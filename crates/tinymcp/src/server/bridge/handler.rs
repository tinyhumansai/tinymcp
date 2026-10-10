//! Async host seams backed by one bounded callback channel.

use crate::server::{
    McpServerHandler, RequestContext, ResourceSpec, ServerInfo, ServerToolSpec, ToolCallError,
};
use futures_util::future::BoxFuture;
use serde_json::{Map, Value};
use std::sync::Arc;
use tinymcp_bus::{ServerCallback, ServerHostCall, ServerHostReply};
use tokio::sync::{mpsc, oneshot};

pub(super) struct WaitingCallback {
    pub callback: ServerCallback,
    pub reply: oneshot::Sender<ServerHostReply>,
}

pub(super) struct CallbackHandler {
    pub info: ServerInfo,
    pub prefix: String,
    pub resources: Vec<ResourceSpec>,
    pub sender: mpsc::Sender<WaitingCallback>,
}

impl CallbackHandler {
    async fn request(&self, call: ServerHostCall) -> Result<Value, ToolCallError> {
        if serde_json::to_vec(&call).map_or(true, |bytes| bytes.len() > super::MAX_BYTES) {
            return Err(ToolCallError::Internal(
                "host callback exceeds byte limit".into(),
            ));
        }
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(WaitingCallback {
                callback: ServerCallback {
                    id: uuid::Uuid::new_v4().to_string(),
                    call,
                },
                reply,
            })
            .await
            .map_err(|_| ToolCallError::Internal("host callback channel closed".into()))?;
        match receiver
            .await
            .map_err(|_| ToolCallError::Internal("host callback cancelled".into()))?
        {
            ServerHostReply::Success { value } => Ok(value),
            ServerHostReply::InvalidParams { message } => {
                Err(ToolCallError::InvalidParams(message))
            }
            ServerHostReply::Internal { message } => Err(ToolCallError::Internal(message)),
            ServerHostReply::ResourceNotFound { message } => {
                Err(ToolCallError::ResourceNotFound(message))
            }
        }
    }
}

pub(super) fn headers(ctx: &RequestContext) -> Map<String, Value> {
    ctx.headers()
        .entries
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect()
}

impl McpServerHandler for Arc<CallbackHandler> {
    fn server_info(&self) -> ServerInfo {
        self.info.clone()
    }
    fn source_type_prefix(&self) -> &str {
        &self.prefix
    }
    fn list_tools<'a>(&'a self, ctx: &'a RequestContext) -> BoxFuture<'a, Vec<ServerToolSpec>> {
        Box::pin(async move { self.list_tools_result(ctx).await.unwrap_or_default() })
    }
    fn list_tools_result<'a>(
        &'a self,
        ctx: &'a RequestContext,
    ) -> BoxFuture<'a, Result<Vec<ServerToolSpec>, ToolCallError>> {
        Box::pin(async move {
            let value = self
                .request(ServerHostCall::ListTools {
                    source_type: ctx.source_type().into(),
                    headers: headers(ctx),
                })
                .await?;
            serde_json::from_value(value).map_err(|_| {
                ToolCallError::Internal("host returned invalid tool declarations".into())
            })
        })
    }
    fn call_tool<'a>(
        &'a self,
        ctx: &'a RequestContext,
        name: &'a str,
        arguments: Map<String, Value>,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async move {
            self.request(ServerHostCall::CallTool {
                source_type: ctx.source_type().into(),
                headers: headers(ctx),
                name: name.into(),
                arguments,
            })
            .await
        })
    }
    fn list_resources(&self) -> Vec<ResourceSpec> {
        self.resources.clone()
    }
    fn read_resource_context<'a>(
        &'a self,
        ctx: &'a RequestContext,
        uri: &'a str,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async move {
            self.request(ServerHostCall::ReadResource {
                source_type: ctx.source_type().into(),
                headers: headers(ctx),
                uri: uri.into(),
            })
            .await
        })
    }
    fn supports_prompts(&self) -> bool {
        true
    }
    fn list_prompts<'a>(
        &'a self,
        ctx: &'a RequestContext,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async move {
            self.request(ServerHostCall::ListPrompts {
                source_type: ctx.source_type().into(),
                headers: headers(ctx),
            })
            .await
        })
    }
    fn get_prompt<'a>(
        &'a self,
        ctx: &'a RequestContext,
        name: &'a str,
        arguments: Map<String, Value>,
    ) -> BoxFuture<'a, Result<Value, ToolCallError>> {
        Box::pin(async move {
            self.request(ServerHostCall::GetPrompt {
                source_type: ctx.source_type().into(),
                headers: headers(ctx),
                name: name.into(),
                arguments,
            })
            .await
        })
    }
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
