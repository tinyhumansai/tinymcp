//! Tests for the structured outcome `mcp_call_tool` attaches to its results:
//! what it reports for an answered call and for each way a call fails, that
//! the model-facing rendering never carries it, and that scrubbing keeps it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse as _;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tinymcp_bus::{
    MCP_CALL_RESULT_KIND, MCP_RESULT_KIND, McpAuthConfig, McpCallOutcome, McpClientConfig,
    McpServerConfig, errors,
};
use tinytools::{Tool, ToolResult};

use super::{ActGate, McpCallTool};
use crate::McpServerRegistry;

const SECRET: &str = "outcome-Secret_77";

fn registry(endpoint: &str, auth: McpAuthConfig, disallowed: &[&str]) -> Arc<McpServerRegistry> {
    Arc::new(
        McpServerRegistry::from_config(&McpClientConfig {
            servers: vec![McpServerConfig {
                name: "docs".into(),
                endpoint: endpoint.into(),
                auth,
                disallowed_tools: disallowed.iter().map(|tool| (*tool).to_string()).collect(),
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        })
        .unwrap(),
    )
}

fn call_tool(registry: Arc<McpServerRegistry>) -> McpCallTool {
    let gate: ActGate = Arc::new(|_| Ok(()));
    McpCallTool::new(registry, gate)
}

fn outcome_of(result: &ToolResult) -> McpCallOutcome {
    McpCallOutcome::from_metadata(result.metadata.as_ref().expect("metadata is attached"))
        .expect("metadata decodes as a call outcome")
}

async fn answering_server() -> String {
    async fn handle(Json(body): Json<Value>) -> axum::response::Response {
        let result = match body["method"].as_str().unwrap_or_default() {
            "initialize" => json!({
                "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "echo", "version": "1.0.0" },
            }),
            "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
            "tools/list" => json!({
                "tools": [{ "name": "whoami", "inputSchema": { "type": "object" } }]
            }),
            "tools/call" => json!({
                "content": [{ "type": "text", "text": format!("you sent {SECRET}") }],
                "isError": false,
            }),
            _ => json!({}),
        };
        Json(json!({ "jsonrpc": "2.0", "id": body["id"].clone(), "result": result }))
            .into_response()
    }
    serve(Router::new().route("/mcp", post(handle))).await
}

async fn unauthorized_server(resource_metadata: Option<&'static str>) -> String {
    let app = Router::new().fallback(move || async move {
        let mut headers = HeaderMap::new();
        let challenge = resource_metadata.map_or_else(
            || "Bearer realm=\"mcp\"".to_string(),
            |url| format!("Bearer resource_metadata=\"{url}\""),
        );
        headers.insert(
            "www-authenticate",
            HeaderValue::from_str(&challenge).unwrap(),
        );
        (StatusCode::UNAUTHORIZED, headers, "").into_response()
    });
    serve(app).await
}

async fn self_authorizing_server() -> String {
    let app = Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(|headers: HeaderMap| async move {
                let host = headers
                    .get("host")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                Json(json!({
                    "issuer": format!("http://{host}/"),
                    "authorization_endpoint": format!("http://{host}/authorize"),
                    "token_endpoint": format!("http://{host}/token"),
                    "registration_endpoint": format!("http://{host}/register"),
                }))
            }),
        )
        .route(
            "/mcp",
            post(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    [("www-authenticate", "Bearer error=\"invalid_token\"")],
                    "",
                )
                    .into_response()
            }),
        );
    serve(app).await
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/mcp")
}

async fn closed_endpoint() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}/mcp")
}

fn args(tool: &str) -> Value {
    json!({ "server": "docs", "tool": tool, "arguments": {} })
}

#[tokio::test]
async fn an_answered_call_reports_ok_with_no_error() {
    let registry = registry(&answering_server().await, McpAuthConfig::None, &[]);
    let result = call_tool(registry).execute(args("whoami")).await.unwrap();
    assert!(!result.is_error, "{}", result.output());
    assert_eq!(
        outcome_of(&result),
        McpCallOutcome::answered("docs", "whoami")
    );
    assert_eq!(
        result.metadata.as_ref().unwrap()["kind"],
        json!(MCP_RESULT_KIND)
    );
    assert_eq!(
        result.metadata.as_ref().unwrap()["outcome"]["kind"],
        json!(MCP_CALL_RESULT_KIND)
    );
}

#[tokio::test]
async fn the_outcome_survives_scrubbing_and_never_reaches_the_rendering() {
    let registry = registry(
        &answering_server().await,
        McpAuthConfig::BearerToken {
            token: SECRET.into(),
        },
        &[],
    );
    let result = call_tool(registry).execute(args("whoami")).await.unwrap();
    assert!(result.output().contains(super::scrub::REDACTED));
    assert!(!result.output().contains(SECRET));
    assert!(outcome_of(&result).ok);
    assert!(!result.output().contains(MCP_CALL_RESULT_KIND));
}

#[tokio::test]
async fn a_blocked_tool_reports_tool_not_allowed() {
    let registry = registry(&answering_server().await, McpAuthConfig::None, &["wipe"]);
    let result = call_tool(registry).execute(args("wipe")).await.unwrap();
    assert!(result.is_error);
    let outcome = outcome_of(&result);
    assert!(!outcome.ok);
    assert_eq!(outcome.tool, "wipe");
    let error = outcome.error.unwrap();
    assert_eq!(error.code, errors::TOOL_NOT_ALLOWED);
    assert!(!error.unauthorized);
    assert!(!error.advertises_oauth);
}

#[tokio::test]
async fn a_401_advertising_oauth_reports_both_flags() {
    let endpoint = unauthorized_server(Some("https://auth.example/.well-known/res")).await;
    let result = call_tool(registry(&endpoint, McpAuthConfig::None, &[]))
        .execute(args("whoami"))
        .await
        .unwrap();
    assert!(result.is_error);
    let error = outcome_of(&result).error.unwrap();
    assert_eq!(error.code, errors::UNAUTHORIZED);
    assert!(error.unauthorized);
    assert!(error.advertises_oauth);
}

#[tokio::test]
async fn a_401_with_no_oauth_metadata_anywhere_is_unauthorized_without_oauth() {
    let endpoint = unauthorized_server(None).await;
    let result = call_tool(registry(&endpoint, McpAuthConfig::None, &[]))
        .execute(args("whoami"))
        .await
        .unwrap();
    let error = outcome_of(&result).error.unwrap();
    assert_eq!(error.code, errors::UNAUTHORIZED);
    assert!(error.unauthorized);
    assert!(!error.advertises_oauth);
}

#[tokio::test]
async fn a_401_whose_origin_publishes_authorization_server_metadata_advertises_oauth() {
    let endpoint = self_authorizing_server().await;
    let result = call_tool(registry(&endpoint, McpAuthConfig::None, &[]))
        .execute(args("whoami"))
        .await
        .unwrap();
    let error = outcome_of(&result).error.unwrap();
    assert_eq!(error.code, errors::UNAUTHORIZED);
    assert!(error.unauthorized);
    assert!(error.advertises_oauth);
}

#[tokio::test]
async fn an_unknown_server_reports_unknown_server() {
    let registry = registry(&closed_endpoint().await, McpAuthConfig::None, &[]);
    let result = call_tool(registry)
        .execute(json!({ "server": "nope", "tool": "whoami", "arguments": {} }))
        .await
        .unwrap();
    let outcome = outcome_of(&result);
    assert_eq!(outcome.server, "nope");
    assert_eq!(outcome.error.unwrap().code, errors::UNKNOWN_SERVER);
}

#[tokio::test]
async fn an_unreachable_server_reports_a_transport_error() {
    let registry = registry(&closed_endpoint().await, McpAuthConfig::None, &[]);
    let result = call_tool(registry).execute(args("whoami")).await.unwrap();
    assert!(result.is_error);
    assert_eq!(outcome_of(&result).error.unwrap().code, errors::TRANSPORT);
}

#[tokio::test]
async fn arguments_that_are_not_an_object_report_invalid_arguments() {
    let registry = registry(&closed_endpoint().await, McpAuthConfig::None, &[]);
    let result = call_tool(registry)
        .execute(json!({ "server": "docs", "tool": "whoami", "arguments": [1] }))
        .await
        .unwrap();
    assert!(result.is_error);
    let outcome = outcome_of(&result);
    assert_eq!(outcome.tool, "whoami");
    assert_eq!(outcome.error.unwrap().code, errors::INVALID_ARGUMENTS);
}

#[tokio::test]
async fn the_outcome_names_the_caller_s_server_and_tool_even_when_they_match_a_secret() {
    let registry = registry(
        &answering_server().await,
        McpAuthConfig::BearerToken {
            token: SECRET.into(),
        },
        &[SECRET],
    );
    let result = call_tool(registry).execute(args(SECRET)).await.unwrap();
    assert!(result.is_error);
    assert!(!result.output().contains(SECRET));
    let outcome = outcome_of(&result);
    assert_eq!(outcome.tool, SECRET);
    assert_eq!(outcome.error.unwrap().code, errors::TOOL_NOT_ALLOWED);
}
