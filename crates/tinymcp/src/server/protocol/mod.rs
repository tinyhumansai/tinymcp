//! JSON-RPC 2.0 for an MCP server.
//!
//! Framing, batching, notifications, protocol version negotiation, method
//! routing and every error shape live here, so a host's
//! [`McpServerHandler`] only ever sees a validated call.
//!
//! Methods served:
//!
//! | Method | Answer |
//! | --- | --- |
//! | `initialize` | negotiated version, fixed capabilities, [`ServerInfo`] |
//! | `ping` | `{}` |
//! | `tools/list` | [`McpServerHandler::list_tools`] |
//! | `tools/call` | [`McpServerHandler::call_tool`] |
//! | `resources/list` | [`McpServerHandler::list_resources`] |
//! | `resources/templates/list` | always empty |
//! | `resources/read` | [`McpServerHandler::read_resource`] |
//!
//! Anything else is `-32601 Method not found`. A message without an `id` is a
//! notification and is never answered.

use crate::{ResourceSpecExt, ServerToolSpecExt};
use serde_json::{Map, Value, json};
use tinymcp_bus::{LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS};

use super::types::{INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR};
use super::{ClientSession, McpServerHandler, RequestContext, RequestHeaders, ServerInfo};

/// Answers one newline-delimited JSON-RPC line.
///
/// Returns the response line — an object for a single answer, an array for a
/// batch that answered more than once — or `None` when nothing needs an
/// answer (a notification, or a batch of them).
pub async fn handle_line(
    handler: &dyn McpServerHandler,
    session: &mut ClientSession,
    headers: &RequestHeaders,
    line: &str,
) -> Option<String> {
    let value = match serde_json::from_str::<Value>(line) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("[mcp_server] parse error: {err}");
            return Some(
                error_response(
                    Value::Null,
                    PARSE_ERROR,
                    "Parse error",
                    Some(json!(err.to_string())),
                )
                .to_string(),
            );
        }
    };

    let mut responses = handle_value(handler, session, headers, value).await;
    match responses.len() {
        0 => None,
        1 => responses.pop().map(|response| response.to_string()),
        _ => Some(Value::Array(responses).to_string()),
    }
}

/// Compiled-module framing with a budget charged before retaining each response.
/// Stops dispatch immediately when an envelope (including batch punctuation)
/// exceeds the budget. Library transports retain their existing unbounded API.
pub(crate) async fn handle_line_bounded(
    handler: &dyn McpServerHandler,
    session: &mut ClientSession,
    headers: &RequestHeaders,
    line: &str,
    limit: usize,
) -> Result<Option<String>, ()> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        let response = handle_line(handler, session, headers, line).await;
        return if response.as_ref().is_some_and(|line| line.len() > limit) {
            Err(())
        } else {
            Ok(response)
        };
    };
    let empty_batch = value.as_array().is_some_and(Vec::is_empty);
    let items = match value {
        Value::Array(items) if !items.is_empty() => items,
        other => vec![other],
    };
    let mut output = LimitedOutput {
        bytes: Vec::new(),
        limit,
    };
    let mut count = 0;
    for item in items {
        let response = if empty_batch {
            Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "Invalid Request",
                Some(json!("batch must not be empty")),
            ))
        } else {
            handle_single_message(handler, session, headers, item).await
        };
        if let Some(response) = response {
            if count == 1 {
                std::io::Write::write_all(&mut output, b"[").map_err(|_| ())?;
                output.bytes.rotate_right(1);
            }
            if count > 0 {
                std::io::Write::write_all(&mut output, b",").map_err(|_| ())?;
            }
            serde_json::to_writer(&mut output, &response).map_err(|_| ())?;
            count += 1;
            if count > 1 && output.bytes.len() == limit {
                return Err(());
            }
        }
    }
    if count == 0 {
        return Ok(None);
    }
    if count > 1 {
        std::io::Write::write_all(&mut output, b"]").map_err(|_| ())?;
    }
    String::from_utf8(output.bytes).map(Some).map_err(|_| ())
}

struct LimitedOutput {
    bytes: Vec<u8>,
    limit: usize,
}
impl std::io::Write for LimitedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("server output exceeds byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Answers one parsed JSON-RPC message or batch.
///
/// Returns one response per request, in order; notifications contribute
/// nothing. An empty batch is itself an invalid request.
pub async fn handle_value(
    handler: &dyn McpServerHandler,
    session: &mut ClientSession,
    headers: &RequestHeaders,
    value: Value,
) -> Vec<Value> {
    match value {
        Value::Array(items) if items.is_empty() => {
            vec![error_response(
                Value::Null,
                INVALID_REQUEST,
                "Invalid Request",
                Some(json!("batch must not be empty")),
            )]
        }
        Value::Array(items) => {
            let mut responses = Vec::new();
            for item in items {
                if let Some(response) = handle_single_message(handler, session, headers, item).await
                {
                    responses.push(response);
                }
            }
            responses
        }
        other => handle_single_message(handler, session, headers, other)
            .await
            .into_iter()
            .collect(),
    }
}

async fn handle_single_message(
    handler: &dyn McpServerHandler,
    session: &mut ClientSession,
    headers: &RequestHeaders,
    value: Value,
) -> Option<Value> {
    let Some(object) = value.as_object() else {
        return Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "Invalid Request",
            Some(json!("message must be a JSON object")),
        ));
    };

    let Some(id) = object.get("id").cloned() else {
        handle_notification(object);
        return None;
    };

    if !valid_request_id(&id) {
        return Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "Invalid Request",
            Some(json!("id must be a string or integer")),
        ));
    }

    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(error_response(
            id,
            INVALID_REQUEST,
            "Invalid Request",
            Some(json!("jsonrpc must be \"2.0\"")),
        ));
    }

    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(error_response(
            id,
            INVALID_REQUEST,
            "Invalid Request",
            Some(json!("method must be a string")),
        ));
    };

    let params = object.get("params").cloned().unwrap_or(Value::Null);
    Some(handle_request(handler, session, headers, id, method, params).await)
}

fn handle_notification(object: &Map<String, Value>) {
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    match method {
        "notifications/initialized" => {
            tracing::debug!("[mcp_server] initialized notification received");
        }
        "notifications/cancelled" => {
            tracing::debug!("[mcp_server] cancelled notification received");
        }
        other => {
            tracing::debug!("[mcp_server] ignoring notification method={other}");
        }
    }
}

async fn handle_request(
    handler: &dyn McpServerHandler,
    session: &mut ClientSession,
    headers: &RequestHeaders,
    id: Value,
    method: &str,
    params: Value,
) -> Value {
    let request_id = id.to_string();
    match method {
        "initialize" => {
            session.observe_initialize_params(&params);
            tracing::debug!(
                "[mcp_server] initialize request id={} client_source_type={}",
                request_id,
                session.source_type()
            );
            let mut result = initialize_result(&handler.server_info(), &params);
            if handler.supports_prompts() {
                result["capabilities"]["prompts"] = json!({});
            }
            success_response(id, result)
        }
        "ping" => success_response(id, json!({})),
        "tools/list" => {
            let ctx = RequestContext::new(session.source_type(), headers.clone());
            let tools = match handler.list_tools_result(&ctx).await {
                Ok(tools) => tools,
                Err(error) => {
                    return error_response(
                        id,
                        error.code(),
                        error.jsonrpc_message(),
                        Some(json!(error.message())),
                    );
                }
            };
            let tools = tools
                .iter()
                .map(super::ServerToolSpec::to_json)
                .collect::<Vec<_>>();
            success_response(id, json!({ "tools": tools }))
        }
        "resources/list" => {
            tracing::debug!("[mcp_server] resources/list request id={request_id}");
            let resources = handler
                .list_resources()
                .iter()
                .map(super::ResourceSpec::to_json)
                .collect::<Vec<_>>();
            tracing::debug!("[mcp_server] resources/list count={}", resources.len());
            success_response(id, json!({ "resources": resources }))
        }
        "resources/templates/list" => {
            // Every advertised URI is concrete. Answering (rather than
            // `-32601`) keeps a client that probes this after seeing the
            // `resources` capability from treating the server as broken.
            tracing::debug!("[mcp_server] resources/templates/list request id={request_id}");
            success_response(id, json!({ "resourceTemplates": [] }))
        }
        "resources/read" => {
            tracing::debug!("[mcp_server] resources/read request id={request_id}");
            let ctx = RequestContext::new(session.source_type(), headers.clone());
            read_resource(handler, &ctx, id, &params).await
        }
        "prompts/list" if handler.supports_prompts() => {
            let ctx = RequestContext::new(session.source_type(), headers.clone());
            handler_response(id, handler.list_prompts(&ctx).await)
        }
        "prompts/get" if handler.supports_prompts() => {
            let ctx = RequestContext::new(session.source_type(), headers.clone());
            get_prompt(handler, &ctx, id, params).await
        }
        "tools/call" => {
            let ctx = RequestContext::new(session.source_type(), headers.clone());
            call_tool(handler, &ctx, id, params).await
        }
        other => error_response(
            id,
            METHOD_NOT_FOUND,
            "Method not found",
            Some(json!(format!("unsupported MCP method `{other}`"))),
        ),
    }
}

async fn get_prompt(
    handler: &dyn McpServerHandler,
    ctx: &RequestContext,
    id: Value,
    params: Value,
) -> Value {
    let Some(name) = params
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return error_response(
            id,
            INVALID_PARAMS,
            "Invalid params",
            Some(json!("prompts/get requires a non-empty name")),
        );
    };
    let arguments = match params.get("arguments") {
        None => Map::new(),
        Some(Value::Object(arguments)) if arguments.values().all(Value::is_string) => {
            arguments.clone()
        }
        _ => {
            return error_response(
                id,
                INVALID_PARAMS,
                "Invalid params",
                Some(json!("prompt arguments must be a string-valued object")),
            );
        }
    };
    handler_response(id, handler.get_prompt(ctx, name, arguments).await)
}

async fn read_resource(
    handler: &dyn McpServerHandler,
    ctx: &RequestContext,
    id: Value,
    params: &Value,
) -> Value {
    let Some(uri) = params
        .get("uri")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|uri| !uri.is_empty())
    else {
        return error_response(
            id,
            INVALID_PARAMS,
            "Invalid params",
            Some(json!(
                "resources/read params.uri must be a non-empty string"
            )),
        );
    };
    match handler.read_resource_context(ctx, uri).await {
        Ok(result) => success_response(id, result),
        Err(err) => {
            tracing::debug!(
                "[mcp_server] resources/read rejected uri={uri} code={}",
                err.code()
            );
            error_response(
                id,
                err.code(),
                err.jsonrpc_message(),
                Some(json!(err.message())),
            )
        }
    }
}

async fn call_tool(
    handler: &dyn McpServerHandler,
    ctx: &RequestContext,
    id: Value,
    params: Value,
) -> Value {
    let request_id = id.to_string();
    let (name, arguments) = match parse_tool_call_params(&params) {
        Ok(parsed) => parsed,
        Err(message) => {
            tracing::debug!(
                "[mcp_server] tools/call params rejected id={} client_source_type={} error={message}",
                request_id,
                ctx.source_type()
            );
            return error_response(id, INVALID_PARAMS, "Invalid params", Some(json!(message)));
        }
    };
    tracing::debug!(
        "[mcp_server] tools/call request id={} tool={} client_source_type={} arg_keys={:?}",
        request_id,
        name,
        ctx.source_type(),
        sorted_keys(&arguments)
    );
    match handler.call_tool(ctx, &name, arguments).await {
        Ok(result) => {
            // Computed outside the macro: `tracing`'s expansion shadows
            // `Value` with its own trait of that name.
            let is_error = result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            tracing::debug!(
                "[mcp_server] tools/call response id={} tool={} client_source_type={} is_error={}",
                request_id,
                name,
                ctx.source_type(),
                is_error
            );
            success_response(id, result)
        }
        Err(err) => {
            // The variant picks the code: caller-fixable problems stay
            // `-32602`, server-side failures are `-32603`, so a client does
            // not blame the caller's arguments for the server's fault.
            tracing::debug!(
                "[mcp_server] tools/call rejected id={} tool={} client_source_type={} code={} error={}",
                request_id,
                name,
                ctx.source_type(),
                err.code(),
                err.message()
            );
            error_response(
                id,
                err.code(),
                err.jsonrpc_message(),
                Some(json!(err.message())),
            )
        }
    }
}

fn handler_response(id: Value, result: Result<Value, super::ToolCallError>) -> Value {
    match result {
        Ok(value) => success_response(id, value),
        Err(error) => error_response(
            id,
            error.code(),
            error.jsonrpc_message(),
            Some(json!(error.message())),
        ),
    }
}

fn initialize_result(info: &ServerInfo, params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let protocol_version = requested
        .filter(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(LATEST_PROTOCOL_VERSION);

    tracing::debug!(
        "[mcp_server] initialize requested_protocol={:?} selected_protocol={}",
        requested,
        protocol_version
    );

    let mut result = json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": {},
            "resources": {
                "subscribe": false,
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": info.name,
            "version": info.version
        },
    });
    if let (Some(instructions), Some(object)) = (&info.instructions, result.as_object_mut()) {
        object.insert("instructions".to_string(), json!(instructions));
    }
    result
}

/// Reads `tools/call` params into a tool name and an arguments object.
///
/// Absent or `null` arguments are `{}`, and an object JSON-encoded into a
/// string is decoded; anything else is refused naming what arrived.
fn parse_tool_call_params(params: &Value) -> Result<(String, Map<String, Value>), String> {
    let object = params
        .as_object()
        .ok_or_else(|| "tools/call params must be an object".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "tools/call params.name must be a non-empty string".to_string())?;
    let arguments = crate::normalize_tool_arguments(object.get("arguments").cloned())
        .map_err(|error| format!("tools/call params.arguments: {error}"))?;
    Ok((name.to_string(), arguments))
}

fn sorted_keys(object: &Map<String, Value>) -> Vec<&str> {
    let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}

fn success_response(id: Value, result: Value) -> Value {
    envelope(id, "result", result)
}

fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = Map::new();
    error.insert("code".to_string(), Value::from(code));
    error.insert("message".to_string(), Value::String(message.to_string()));
    if let Some(data) = data {
        error.insert("data".to_string(), data);
    }
    envelope(id, "error", Value::Object(error))
}

/// A JSON-RPC 2.0 response carrying `body` under `kind` (`result`/`error`).
fn envelope(id: Value, kind: &str, body: Value) -> Value {
    let mut response = Map::new();
    response.insert("jsonrpc".to_string(), Value::from("2.0"));
    response.insert("id".to_string(), id);
    response.insert(kind.to_string(), body);
    Value::Object(response)
}

fn valid_request_id(id: &Value) -> bool {
    match id {
        Value::String(_) => true,
        Value::Number(n) => n.as_i64().is_some() || n.as_u64().is_some(),
        _ => false,
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
