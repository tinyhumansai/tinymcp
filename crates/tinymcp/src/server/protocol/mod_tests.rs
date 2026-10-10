//! Unit tests for the JSON-RPC protocol layer.
//!
//! The expectations are whole response lines, byte for byte. These are the
//! same shapes `OpenHuman`'s golden fixtures pinned before the protocol moved
//! here, so a diff in this file is a wire change.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::RequestHeadersExt;
use serde_json::{Value, json};
use tinymcp_bus::{LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS};

use super::{handle_line, handle_value};
use crate::server::fixture::{DemoHandler, ECHOED_HEADER};
use crate::server::{ClientSession, RequestHeaders};

const DEMO: DemoHandler = DemoHandler::new();

async fn line(input: &str) -> Option<String> {
    let mut session = ClientSession::new("demo");
    handle_line(&DEMO, &mut session, &RequestHeaders::new(), input).await
}

async fn one(value: Value) -> Value {
    let mut session = ClientSession::new("demo");
    let mut responses = handle_value(&DEMO, &mut session, &RequestHeaders::new(), value).await;
    assert_eq!(responses.len(), 1, "expected one response");
    responses.remove(0)
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_answers_the_handler_identity_and_fixed_capabilities() {
    assert_eq!(
        line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"Claude Desktop"}}}"#).await.as_deref(),
        Some(r#"{"id":1,"jsonrpc":"2.0","result":{"capabilities":{"resources":{"listChanged":false,"subscribe":false},"tools":{}},"instructions":"Use the demo tools.","protocolVersion":"2025-06-18","serverInfo":{"name":"demo-server","version":"9.9.9"}}}"#)
    );
}

#[tokio::test]
async fn initialize_omits_instructions_the_handler_does_not_give() {
    let handler = DemoHandler {
        instructions: false,
    };
    let mut session = ClientSession::new("demo");
    let response = handle_line(
        &handler,
        &mut session,
        &RequestHeaders::new(),
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
    )
    .await;
    assert_eq!(
        response.as_deref(),
        Some(
            r#"{"id":1,"jsonrpc":"2.0","result":{"capabilities":{"resources":{"listChanged":false,"subscribe":false},"tools":{}},"protocolVersion":"2025-11-25","serverInfo":{"name":"demo-server","version":"9.9.9"}}}"#
        )
    );
}

#[tokio::test]
async fn initialize_echoes_every_supported_version_and_falls_back_to_latest() {
    for version in SUPPORTED_PROTOCOL_VERSIONS {
        let response = one(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": version}
        }))
        .await;
        assert_eq!(response["result"]["protocolVersion"], *version);
    }
    for params in [
        json!({"protocolVersion": "1999-01-01"}),
        json!({}),
        json!(null),
    ] {
        let response = one(json!({
            "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": params
        }))
        .await;
        assert_eq!(
            response["result"]["protocolVersion"],
            LATEST_PROTOCOL_VERSION
        );
    }
}

#[tokio::test]
async fn initialize_provenance_reaches_later_tool_calls_on_the_session() {
    let mut session = ClientSession::new("demo");
    let headers = RequestHeaders::new();
    let _ = handle_value(
        &DEMO,
        &mut session,
        &headers,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"clientInfo": {"name": "Claude Desktop"}}}),
    )
    .await;
    assert_eq!(session.source_type(), "demo:claude-desktop");
    let responses = handle_value(
        &DEMO,
        &mut session,
        &headers,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "echo"}}),
    )
    .await;
    assert_eq!(
        responses[0]["result"]["structuredContent"]["source_type"],
        "demo:claude-desktop"
    );
}

// ---------------------------------------------------------------------------
// Framing, batching, notifications
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ping_answers_an_empty_result() {
    assert_eq!(
        line(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#)
            .await
            .as_deref(),
        Some(r#"{"id":"abc","jsonrpc":"2.0","result":{}}"#)
    );
}

#[tokio::test]
async fn notifications_never_answer() {
    for input in [
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/other"}"#,
        r#"{"jsonrpc":"2.0"}"#,
        r#"[{"jsonrpc":"2.0","method":"notifications/initialized"}]"#,
    ] {
        assert_eq!(line(input).await, None, "{input}");
    }
}

#[tokio::test]
async fn malformed_messages_answer_invalid_request() {
    let cases = [
        (
            "[]",
            r#"{"error":{"code":-32600,"data":"batch must not be empty","message":"Invalid Request"},"id":null,"jsonrpc":"2.0"}"#,
        ),
        (
            "42",
            r#"{"error":{"code":-32600,"data":"message must be a JSON object","message":"Invalid Request"},"id":null,"jsonrpc":"2.0"}"#,
        ),
        (
            r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
            r#"{"error":{"code":-32600,"data":"id must be a string or integer","message":"Invalid Request"},"id":null,"jsonrpc":"2.0"}"#,
        ),
        (
            r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#,
            r#"{"error":{"code":-32600,"data":"id must be a string or integer","message":"Invalid Request"},"id":null,"jsonrpc":"2.0"}"#,
        ),
        (
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            r#"{"error":{"code":-32600,"data":"id must be a string or integer","message":"Invalid Request"},"id":null,"jsonrpc":"2.0"}"#,
        ),
        (
            r#"{"id":1,"method":"ping"}"#,
            r#"{"error":{"code":-32600,"data":"jsonrpc must be \"2.0\"","message":"Invalid Request"},"id":1,"jsonrpc":"2.0"}"#,
        ),
        (
            r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
            r#"{"error":{"code":-32600,"data":"jsonrpc must be \"2.0\"","message":"Invalid Request"},"id":1,"jsonrpc":"2.0"}"#,
        ),
        (
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"error":{"code":-32600,"data":"method must be a string","message":"Invalid Request"},"id":1,"jsonrpc":"2.0"}"#,
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":5}"#,
            r#"{"error":{"code":-32600,"data":"method must be a string","message":"Invalid Request"},"id":1,"jsonrpc":"2.0"}"#,
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(line(input).await.as_deref(), Some(expected), "{input}");
    }
}

#[tokio::test]
async fn unsigned_ids_beyond_i64_are_still_valid() {
    let response = one(json!({"jsonrpc": "2.0", "id": u64::MAX, "method": "ping"})).await;
    assert_eq!(response["id"], u64::MAX);
    assert!(response.get("result").is_some(), "{response}");
}

#[tokio::test]
async fn unparseable_lines_answer_a_parse_error_with_a_null_id() {
    assert_eq!(
        line("{not-json").await.as_deref(),
        Some(
            r#"{"error":{"code":-32700,"data":"key must be a string at line 1 column 2","message":"Parse error"},"id":null,"jsonrpc":"2.0"}"#
        )
    );
}

#[tokio::test]
async fn unknown_methods_answer_method_not_found() {
    assert_eq!(
        line(r#"{"jsonrpc":"2.0","id":1,"method":"prompts/list"}"#)
            .await
            .as_deref(),
        Some(
            r#"{"error":{"code":-32601,"data":"unsupported MCP method `prompts/list`","message":"Method not found"},"id":1,"jsonrpc":"2.0"}"#
        )
    );
}

#[tokio::test]
async fn a_batch_answers_its_requests_in_order_as_one_array_line() {
    assert_eq!(
        line(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"},42,{"jsonrpc":"2.0","id":3,"method":"nope"}]"#).await.as_deref(),
        Some(r#"[{"id":1,"jsonrpc":"2.0","result":{}},{"error":{"code":-32600,"data":"message must be a JSON object","message":"Invalid Request"},"id":null,"jsonrpc":"2.0"},{"error":{"code":-32601,"data":"unsupported MCP method `nope`","message":"Method not found"},"id":3,"jsonrpc":"2.0"}]"#)
    );
}

#[tokio::test]
async fn a_batch_of_one_request_answers_a_bare_object_line() {
    assert_eq!(
        line(r#"[{"jsonrpc":"2.0","id":9,"method":"ping"}]"#)
            .await
            .as_deref(),
        Some(r#"{"id":9,"jsonrpc":"2.0","result":{}}"#)
    );
}

// ---------------------------------------------------------------------------
// tools
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tools_list_renders_the_handler_catalog() {
    assert_eq!(
        line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .await
            .as_deref(),
        Some(
            r#"{"id":2,"jsonrpc":"2.0","result":{"tools":[{"annotations":{"readOnlyHint":true},"description":"Echo the arguments.","inputSchema":{"type":"object"},"name":"echo","title":"Echo"},{"description":"No title.","inputSchema":{},"name":"bare"}]}}"#
        )
    );
}

#[tokio::test]
async fn tools_call_params_are_validated_before_the_handler_runs() {
    let cases = [
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#,
            "tools/call params must be an object",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":[]}"#,
            "tools/call params must be an object",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#,
            "tools/call params.name must be a non-empty string",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"   "}}"#,
            "tools/call params.name must be a non-empty string",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":[1,2]}}"#,
            "tools/call params.arguments: tool arguments must be a JSON object, not an array",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":"not json"}}"#,
            "tools/call params.arguments: tool arguments must be a JSON object, not a string that is not JSON",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":7}}"#,
            "tools/call params.arguments: tool arguments must be a JSON object, not a number",
        ),
    ];
    for (input, data) in cases {
        let expected = format!(
            r#"{{"error":{{"code":-32602,"data":{},"message":"Invalid params"}},"id":1,"jsonrpc":"2.0"}}"#,
            Value::String(data.to_string())
        );
        assert_eq!(line(input).await, Some(expected), "{input}");
    }
}

#[tokio::test]
async fn tools_call_hands_the_handler_normalized_arguments() {
    for (arguments, expected_text) in [
        (json!({"a": 1}), r#"{"a":1}"#),
        (json!("{\"a\":1}"), r#"{"a":1}"#),
        (Value::Null, "{}"),
    ] {
        let response = one(json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": " echo ", "arguments": arguments}
        }))
        .await;
        assert_eq!(response["result"]["content"][0]["text"], expected_text);
    }
    let absent = one(json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "echo"}
    }))
    .await;
    assert_eq!(absent["result"]["content"][0]["text"], "{}");
}

#[tokio::test]
async fn tools_call_passes_transport_headers_to_the_handler() {
    let mut headers = RequestHeaders::new();
    headers.insert(ECHOED_HEADER, "3");
    let mut session = ClientSession::new("demo");
    let responses = handle_value(
        &DEMO,
        &mut session,
        &headers,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "echo"}}),
    )
    .await;
    assert_eq!(responses[0]["result"]["structuredContent"]["depth"], "3");
    assert_eq!(
        responses[0]["result"]["structuredContent"]["source_type"],
        "demo"
    );
}

#[tokio::test]
async fn tools_call_answers_results_and_maps_handler_errors_to_codes() {
    let soft = one(json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "soft_error"}
    }))
    .await;
    assert_eq!(soft["result"]["isError"], true);

    let cases = [
        (
            "fail_invalid",
            r#"{"error":{"code":-32602,"data":"bad input","message":"Invalid params"},"id":1,"jsonrpc":"2.0"}"#,
        ),
        (
            "fail_internal",
            r#"{"error":{"code":-32603,"data":"boom","message":"Internal error"},"id":1,"jsonrpc":"2.0"}"#,
        ),
        (
            "no.such_tool",
            r#"{"error":{"code":-32602,"data":"unknown MCP tool `no.such_tool`","message":"Invalid params"},"id":1,"jsonrpc":"2.0"}"#,
        ),
    ];
    for (tool, expected) in cases {
        let input = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool}
        });
        assert_eq!(
            line(&input.to_string()).await.as_deref(),
            Some(expected),
            "{tool}"
        );
    }
}

// ---------------------------------------------------------------------------
// resources
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resources_list_renders_the_handler_catalog() {
    assert_eq!(
        line(r#"{"jsonrpc":"2.0","id":10,"method":"resources/list"}"#)
            .await
            .as_deref(),
        Some(
            r#"{"id":10,"jsonrpc":"2.0","result":{"resources":[{"description":"The readme.","mimeType":"text/markdown","name":"Readme","uri":"demo://readme"}]}}"#
        )
    );
}

#[tokio::test]
async fn resources_read_answers_the_handler_contents() {
    assert_eq!(
        line(r#"{"jsonrpc":"2.0","id":11,"method":"resources/read","params":{"uri":" demo://readme "}}"#).await.as_deref(),
        Some(
            r##"{"id":11,"jsonrpc":"2.0","result":{"contents":[{"mimeType":"text/markdown","text":"# Demo","uri":"demo://readme"}]}}"##
        )
    );
}

#[tokio::test]
async fn resources_read_rejects_unknown_and_missing_uris() {
    assert_eq!(
        line(
            r#"{"jsonrpc":"2.0","id":12,"method":"resources/read","params":{"uri":"demo://nope"}}"#
        )
        .await
        .as_deref(),
        Some(
            r#"{"error":{"code":-32002,"data":"no resource with uri `demo://nope`","message":"Resource not found"},"id":12,"jsonrpc":"2.0"}"#
        )
    );
    for input in [
        r#"{"jsonrpc":"2.0","id":13,"method":"resources/read","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":13,"method":"resources/read","params":{"uri":"  "}}"#,
        r#"{"jsonrpc":"2.0","id":13,"method":"resources/read"}"#,
    ] {
        assert_eq!(
            line(input).await.as_deref(),
            Some(
                r#"{"error":{"code":-32602,"data":"resources/read params.uri must be a non-empty string","message":"Invalid params"},"id":13,"jsonrpc":"2.0"}"#
            ),
            "{input}"
        );
    }
}

#[tokio::test]
async fn resource_templates_are_always_an_empty_list() {
    for input in [
        r#"{"jsonrpc":"2.0","id":14,"method":"resources/templates/list"}"#,
        r#"{"jsonrpc":"2.0","id":14,"method":"resources/templates/list","params":{"cursor":"x"}}"#,
    ] {
        assert_eq!(
            line(input).await.as_deref(),
            Some(r#"{"id":14,"jsonrpc":"2.0","result":{"resourceTemplates":[]}}"#),
            "{input}"
        );
    }
}

#[tokio::test]
async fn bounded_framing_matches_library_shapes_and_charges_envelope_punctuation() {
    for value in [
        json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
        json!([]),
        json!([[], {"jsonrpc":"2.0","id":2,"method":"ping"}]),
        json!([{"method":"notifications/initialized"}]),
        json!([{"jsonrpc":"2.0","id":1,"method":"ping"}, {"jsonrpc":"2.0","id":2,"method":"ping"}]),
    ] {
        let input = value.to_string();
        let expected = line(&input).await;
        let size = expected.as_ref().map_or(0, String::len);
        let actual = super::handle_line_bounded(
            &DEMO,
            &mut ClientSession::new("demo"),
            &RequestHeaders::new(),
            &input,
            size,
        )
        .await
        .unwrap();
        assert_eq!(actual, expected);
        if size > 0 {
            assert!(
                super::handle_line_bounded(
                    &DEMO,
                    &mut ClientSession::new("demo"),
                    &RequestHeaders::new(),
                    &input,
                    size - 1
                )
                .await
                .is_err()
            );
        }
    }
    for limit in [0, 1000] {
        let result = super::handle_line_bounded(
            &DEMO,
            &mut ClientSession::new("demo"),
            &RequestHeaders::new(),
            "{",
            limit,
        )
        .await;
        assert_eq!(result.is_ok(), limit > 0);
    }
    let mut output = super::LimitedOutput {
        bytes: Vec::new(),
        limit: 0,
    };
    assert!(std::io::Write::flush(&mut output).is_ok());
}

struct CountingResources {
    calls: std::sync::atomic::AtomicUsize,
}
impl crate::server::McpServerHandler for CountingResources {
    fn server_info(&self) -> crate::server::ServerInfo {
        DEMO.server_info()
    }
    fn source_type_prefix(&self) -> &'static str {
        "fixture"
    }
    fn list_resources(&self) -> Vec<crate::server::ResourceSpec> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        vec![
            crate::server::ResourceSpec::new("fixture://large", "large")
                .with_description("x".repeat(100)),
        ]
    }
    fn list_tools<'a>(
        &'a self,
        ctx: &'a crate::server::RequestContext,
    ) -> futures_util::future::BoxFuture<'a, Vec<crate::server::ServerToolSpec>> {
        DEMO.list_tools(ctx)
    }
    fn call_tool<'a>(
        &'a self,
        ctx: &'a crate::server::RequestContext,
        name: &'a str,
        arguments: serde_json::Map<String, Value>,
    ) -> futures_util::future::BoxFuture<'a, Result<Value, crate::server::ToolCallError>> {
        DEMO.call_tool(ctx, name, arguments)
    }
    fn read_resource<'a>(
        &'a self,
        uri: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<Value, crate::server::ToolCallError>> {
        DEMO.read_resource(uri)
    }
}

#[tokio::test]
async fn bounded_static_batch_stops_construction_when_second_response_exhausts_budget() {
    let handler = CountingResources {
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let input = json!(vec![
        json!({"jsonrpc":"2.0","id":1,"method":"resources/list"});
        256
    ])
    .to_string();
    let result = super::handle_line_bounded(
        &handler,
        &mut ClientSession::new("fixture"),
        &RequestHeaders::new(),
        &input,
        300,
    )
    .await;
    assert!(result.is_err());
    // Only two small declarations are constructed, rather than buffering all 256.
    assert_eq!(handler.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}
