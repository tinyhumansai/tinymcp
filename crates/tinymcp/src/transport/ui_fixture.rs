//! A loopback MCP server exercising tool `_meta`, resources, and rich tool
//! results, shared by the suites that cover those paths.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use tinymcp_bus::MAX_RESOURCE_BYTES;

/// The HTML the fixture serves at [`CARD_URI`].
pub(crate) const CARD_HTML: &str = "<!doctype html><p>card</p>";
/// A resource the fixture serves.
pub(crate) const CARD_URI: &str = "ui://card";
/// A resource above [`MAX_RESOURCE_BYTES`].
pub(crate) const BIG_URI: &str = "ui://big";
/// The MIME type the fixture declares for [`CARD_URI`].
pub(crate) const APP_MIME: &str = "text/html;profile=mcp-app";
/// The text block the fixture's tool call answers with.
pub(crate) const CALL_TEXT: &str = "card shown";

/// A running fixture.
pub(crate) struct UiFixture {
    /// The endpoint to dial.
    pub endpoint: String,
    /// The `params` of every `initialize` the fixture received.
    pub initialize_params: Arc<Mutex<Vec<Value>>>,
}

/// The `tools/call` reply the fixture sends.
pub(crate) fn call_reply() -> Value {
    json!({
        "content": [
            { "type": "text", "text": CALL_TEXT },
            { "type": "resource", "resource": {
                "uri": CARD_URI, "mimeType": APP_MIME, "text": CARD_HTML,
            } },
        ],
        "structuredContent": { "temperature": 21 },
        "_meta": { "openai/outputTemplate": CARD_URI },
    })
}

/// The `_meta` the fixture lists for its `show_card` tool.
pub(crate) fn show_card_meta() -> Value {
    json!({ "ui": { "resourceUri": CARD_URI } })
}

/// Starts the fixture on a loopback port.
pub(crate) async fn spawn() -> UiFixture {
    let initialize_params = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/", post(handle))
        .with_state(Arc::clone(&initialize_params));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    UiFixture {
        endpoint: format!("http://{addr}/"),
        initialize_params,
    }
}

async fn handle(
    State(initialize_params): State<Arc<Mutex<Vec<Value>>>>,
    Json(body): Json<Value>,
) -> Response {
    let params = body.get("params").cloned().unwrap_or(Value::Null);
    let result = match body["method"].as_str().unwrap_or_default() {
        "initialize" => {
            initialize_params.lock().unwrap().push(params);
            json!({
                "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                "capabilities": { "tools": {}, "resources": {} },
                "serverInfo": { "name": "ui-fixture", "version": "1" },
            })
        }
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "tools/list" => json!({ "tools": [
            { "name": "show_card", "inputSchema": { "type": "object" }, "_meta": show_card_meta() },
            { "name": "plain", "inputSchema": { "type": "object" } },
        ] }),
        "tools/call" => call_reply(),
        "resources/list" => match params.get("cursor").and_then(Value::as_str) {
            None => json!({
                "resources": [{ "uri": CARD_URI, "name": "card", "mimeType": APP_MIME }],
                "nextCursor": "page-2",
            }),
            Some(_) => json!({ "resources": [{ "uri": "ui://other", "name": "other" }] }),
        },
        "resources/read" => match params.get("uri").and_then(Value::as_str) {
            Some(CARD_URI) => json!({
                "contents": [{ "uri": CARD_URI, "mimeType": APP_MIME, "text": CARD_HTML }],
            }),
            Some(BIG_URI) => json!({
                "contents": [{ "uri": BIG_URI, "text": "a".repeat(MAX_RESOURCE_BYTES + 1) }],
            }),
            _ => {
                return Json(json!({
                    "jsonrpc": "2.0",
                    "id": body["id"].clone(),
                    "error": { "code": -32002, "message": "resource not found" },
                }))
                .into_response();
            }
        },
        _ => json!({}),
    };
    Json(json!({ "jsonrpc": "2.0", "id": body["id"].clone(), "result": result })).into_response()
}
