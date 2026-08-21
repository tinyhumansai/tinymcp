//! Unit tests for the Streamable HTTP transport.
//!
//! These stand up a real `axum` server on a loopback port and dial it with a
//! real client. Mocking `reqwest` would let every assertion about headers,
//! redirects, session expiry, and SSE framing pass without any of those things
//! actually working; here the server rejects a request that is missing what the
//! protocol requires, so the assertions have teeth.
//!
//! Each test binds port zero and gets its own server, so the suite is
//! order-independent and safe to run in parallel.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use axum::extract::State;
use axum::http::{HeaderMap as AxumHeaderMap, Method, StatusCode as AxumStatus};
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::headers::parse_www_authenticate_challenge;
use super::sse::{first_complete_sse_data, parse_sse_events};
use super::{HEADER_PROTOCOL_VERSION, HEADER_SESSION_ID, McpHttpClient};
use crate::Error;
use tinymcp_bus::{HttpHeader, LATEST_PROTOCOL_VERSION, McpAuthConfig};

// ---------------------------------------------------------------------------
// Test server
// ---------------------------------------------------------------------------

/// Counters a test asserts against after driving the client.
#[derive(Clone)]
struct TestState {
    init_count: Arc<AtomicUsize>,
    call_count: Arc<AtomicUsize>,
}

impl TestState {
    fn new() -> Self {
        Self {
            init_count: Arc::new(AtomicUsize::new(0)),
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn inits(&self) -> usize {
        self.init_count.load(AtomicOrdering::SeqCst)
    }

    fn calls(&self) -> usize {
        self.call_count.load(AtomicOrdering::SeqCst)
    }
}

/// Binds a loopback port and serves `app`, returning the endpoint.
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/")
}

/// Whether the request asked for both encodings the transport accepts.
fn has_streamable_http_accept(headers: &AxumHeaderMap) -> bool {
    headers
        .get(ACCEPT.as_str())
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.contains("application/json") && value.contains("text/event-stream")
        })
}

/// The main server: a full handshake, one tool, and a session it enforces.
async fn mcp_handler(
    State(state): State<TestState>,
    headers: AxumHeaderMap,
    method: Method,
    Json(body): Json<Value>,
) -> AxumResponse {
    if method == Method::POST && !has_streamable_http_accept(&headers) {
        return (AxumStatus::NOT_ACCEPTABLE, "missing the mcp accept header").into_response();
    }

    let rpc_method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();

    if method == Method::POST && rpc_method == "initialize" {
        state.init_count.fetch_add(1, AtomicOrdering::SeqCst);
        return (
            [(HEADER_SESSION_ID, "session-1")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": body["id"].clone(),
                "result": {
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": { "tools": { "listChanged": true } },
                    "serverInfo": { "name": "test-server", "version": "1.0.0" },
                },
            })),
        )
            .into_response();
    }

    // Everything after the handshake must carry the session and the negotiated
    // version. Enforcing it here is what makes the header assertions real.
    if headers
        .get(HEADER_SESSION_ID)
        .and_then(|value| value.to_str().ok())
        != Some("session-1")
    {
        return (AxumStatus::BAD_REQUEST, "missing or invalid session").into_response();
    }
    if headers
        .get(HEADER_PROTOCOL_VERSION)
        .and_then(|value| value.to_str().ok())
        != Some(LATEST_PROTOCOL_VERSION)
    {
        return (AxumStatus::BAD_REQUEST, "missing the protocol version").into_response();
    }

    match rpc_method {
        "notifications/initialized" => AxumStatus::NO_CONTENT.into_response(),
        "tools/list" => Json(json!({
            "jsonrpc": "2.0",
            "id": body["id"].clone(),
            "result": {
                "tools": [{
                    "name": "needs_header",
                    "description": "needs an x-mcp-header",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "tenant": { "type": "string", "x-mcp-header": "tenant" },
                        },
                    },
                }],
            },
        }))
        .into_response(),
        "tools/call" => {
            state.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            if headers
                .get("Mcp-Param-tenant")
                .and_then(|value| value.to_str().ok())
                != Some("acme")
            {
                return (
                    AxumStatus::BAD_REQUEST,
                    "missing the mirrored tenant header",
                )
                    .into_response();
            }
            Json(json!({
                "jsonrpc": "2.0",
                "id": body["id"].clone(),
                "result": { "content": [{ "type": "text", "text": "remote result" }] },
            }))
            .into_response()
        }
        other => (
            AxumStatus::BAD_REQUEST,
            format!("unexpected method {other}"),
        )
            .into_response(),
    }
}

/// The `GET` half: one SSE frame.
async fn events_handler(headers: AxumHeaderMap) -> AxumResponse {
    let accepts_sse = headers
        .get(ACCEPT.as_str())
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));
    if !accepts_sse {
        return (AxumStatus::NOT_ACCEPTABLE, "missing the sse accept header").into_response();
    }
    if headers.get(HEADER_SESSION_ID).is_none() {
        return (AxumStatus::BAD_REQUEST, "no session").into_response();
    }

    (
        [(CONTENT_TYPE.as_str(), "text/event-stream")],
        "id: 1\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{\"ok\":true}}\n\n",
    )
        .into_response()
}

async fn delete_handler() -> AxumResponse {
    AxumStatus::NO_CONTENT.into_response()
}

async fn spawn_test_server() -> (String, TestState) {
    let state = TestState::new();
    let app = Router::new()
        .route(
            "/",
            post(mcp_handler).get(events_handler).delete(delete_handler),
        )
        .with_state(state.clone());
    (serve(app).await, state)
}

/// A server that expires the session once, to exercise the retry.
async fn retrying_mcp_handler(
    State(state): State<TestState>,
    headers: AxumHeaderMap,
    Json(body): Json<Value>,
) -> AxumResponse {
    if !has_streamable_http_accept(&headers) {
        return (AxumStatus::NOT_ACCEPTABLE, "missing the mcp accept header").into_response();
    }

    let rpc_method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match rpc_method {
        "initialize" => {
            state.init_count.fetch_add(1, AtomicOrdering::SeqCst);
            (
                [(HEADER_SESSION_ID, "session-retry")],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": body["id"].clone(),
                    "result": {
                        "protocolVersion": LATEST_PROTOCOL_VERSION,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "retry-server", "version": "1.0.0" },
                    },
                })),
            )
                .into_response()
        }
        "notifications/initialized" => AxumStatus::NO_CONTENT.into_response(),
        "tools/list" => {
            let attempt = state.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            let holds_session = headers
                .get(HEADER_SESSION_ID)
                .and_then(|value| value.to_str().ok())
                == Some("session-retry");
            if attempt == 0 && holds_session {
                return (AxumStatus::NOT_FOUND, "expired").into_response();
            }
            Json(json!({
                "jsonrpc": "2.0",
                "id": body["id"].clone(),
                "result": { "tools": [] },
            }))
            .into_response()
        }
        _ => (AxumStatus::BAD_REQUEST, "unexpected").into_response(),
    }
}

async fn spawn_retry_server() -> (String, TestState) {
    let state = TestState::new();
    let app = Router::new()
        .route("/", post(retrying_mcp_handler))
        .with_state(state.clone());
    (serve(app).await, state)
}

/// A server that 401s with a challenge and publishes both discovery documents.
async fn spawn_discovery_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let challenge = format!(
        "Bearer realm=\"mcp\", resource_metadata=\"{base}/.well-known/oauth-protected-resource\""
    );
    let resource_base = base.clone();
    let issuer_base = base.clone();

    let app = Router::new()
        .route(
            "/",
            post(move || {
                let challenge = challenge.clone();
                async move {
                    (
                        AxumStatus::UNAUTHORIZED,
                        [("WWW-Authenticate", challenge.as_str())],
                        "",
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(move || {
                let base = resource_base.clone();
                async move {
                    Json(json!({
                        "resource": format!("{base}/"),
                        "authorization_servers": [base],
                        "scopes_supported": ["mcp:tools"],
                    }))
                }
            }),
        )
        .route(
            "/.well-known/openid-configuration",
            get(move || {
                let base = issuer_base.clone();
                async move {
                    Json(json!({
                        "issuer": base,
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "grant_types_supported": ["authorization_code"],
                        "code_challenge_methods_supported": ["S256"],
                    }))
                }
            }),
        );

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/")
}

/// A server that answers only when the request carries what `expected` names.
fn credential_gated_server(
    server_name: &'static str,
    expected: Vec<(&'static str, &'static str)>,
) -> Router {
    Router::new().route(
        "/",
        post(move |headers: AxumHeaderMap, Json(body): Json<Value>| {
            let expected = expected.clone();
            async move {
                let satisfied = expected.iter().all(|(name, value)| {
                    headers
                        .get(*name)
                        .and_then(|found| found.to_str().ok())
                        .is_some_and(|found| found == *value)
                });
                if !satisfied {
                    return (AxumStatus::UNAUTHORIZED, "missing a credential").into_response();
                }
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": body["id"].clone(),
                    "result": {
                        "protocolVersion": LATEST_PROTOCOL_VERSION,
                        "capabilities": {},
                        "serverInfo": { "name": server_name, "version": "1.0.0" },
                    },
                }))
                .into_response()
            }
        }),
    )
}

// ---------------------------------------------------------------------------
// Handshake, tools, and sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_and_list_tools_negotiate_one_session() {
    let (endpoint, state) = spawn_test_server().await;
    let client = McpHttpClient::new(endpoint, 5).unwrap();

    let tools = client.list_tools().await.expect("list_tools");

    assert_eq!(tools.len(), 1);
    assert_eq!(state.inits(), 1, "the handshake ran more than once");
    assert_eq!(
        client
            .initialize_snapshot()
            .expect("a snapshot")
            .protocol_version,
        LATEST_PROTOCOL_VERSION
    );
}

#[tokio::test]
async fn a_second_initialize_reuses_the_first_handshake() {
    let (endpoint, state) = spawn_test_server().await;
    let client = McpHttpClient::new(endpoint, 5).unwrap();

    client.initialize().await.expect("first");
    client.initialize().await.expect("second");

    assert_eq!(state.inits(), 1);
}

#[tokio::test]
async fn calling_a_tool_mirrors_its_schema_tagged_arguments_into_headers() {
    // The server 400s unless `Mcp-Param-tenant` arrives, so this passes only if
    // the mirroring actually reached the wire.
    let (endpoint, state) = spawn_test_server().await;
    let client = McpHttpClient::new(endpoint, 5).unwrap();

    let result = client
        .call_tool("needs_header", json!({ "tenant": "acme" }))
        .await
        .expect("call_tool");

    assert_eq!(result.rendered.output(), "remote result");
    assert!(!result.rendered.is_error);
    assert_eq!(state.calls(), 1);
}

#[tokio::test]
async fn calling_a_tool_keeps_the_raw_reply_beside_the_rendering() {
    let (endpoint, _) = spawn_test_server().await;
    let client = McpHttpClient::new(endpoint, 5).unwrap();

    let result = client
        .call_tool("needs_header", json!({ "tenant": "acme" }))
        .await
        .expect("call_tool");

    assert_eq!(result.raw_result["content"][0]["text"], "remote result");
}

#[tokio::test]
async fn a_404_while_holding_a_session_reinitializes_and_retries_once() {
    let (endpoint, state) = spawn_retry_server().await;
    let client = McpHttpClient::new(endpoint, 5).unwrap();

    let tools = client.list_tools().await.expect("list_tools");

    assert!(tools.is_empty());
    assert_eq!(state.inits(), 2, "the session was not reinitialized");
    assert_eq!(state.calls(), 2, "the request was not retried exactly once");
}

#[tokio::test]
async fn draining_events_parses_the_sse_stream() {
    let (endpoint, _) = spawn_test_server().await;
    let client = McpHttpClient::new(endpoint, 5).unwrap();

    let events = client.drain_events(None).await.expect("drain_events");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id.as_deref(), Some("1"));
    assert_eq!(events[0].event.as_deref(), Some("message"));
    assert_eq!(events[0].data.as_ref().unwrap()["params"]["ok"], true);
}

#[tokio::test]
async fn closing_the_session_sends_a_delete_and_clears_local_state() {
    let (endpoint, _) = spawn_test_server().await;
    let client = McpHttpClient::new(endpoint, 5).unwrap();

    client.initialize().await.expect("initialize");
    client.close_session().await.expect("close_session");

    assert!(client.initialize_snapshot().is_none());
}

#[tokio::test]
async fn closing_a_session_that_was_never_opened_does_nothing() {
    let (endpoint, _) = spawn_test_server().await;
    let client = McpHttpClient::new(endpoint, 5).unwrap();

    // No handshake ran, so there is no session id and no request to send.
    client.close_session().await.expect("close_session");
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_bearer_token_reaches_the_wire() {
    let endpoint = serve(credential_gated_server(
        "bearer-server",
        vec![("authorization", "Bearer secret-token")],
    ))
    .await;

    let client = McpHttpClient::builder(endpoint)
        .timeout_secs(2)
        .auth(McpAuthConfig::BearerToken {
            token: "secret-token".into(),
        })
        .build()
        .unwrap();

    let initialized = client.initialize().await.expect("initialize");
    assert_eq!(initialized.server_info["name"], "bearer-server");
}

#[tokio::test]
async fn a_bearer_token_is_trimmed_before_it_is_sent() {
    // Users paste tokens, and a pasted token routinely carries whitespace.
    let endpoint = serve(credential_gated_server(
        "bearer-server",
        vec![("authorization", "Bearer secret-token")],
    ))
    .await;

    let client = McpHttpClient::builder(endpoint)
        .timeout_secs(2)
        .auth(McpAuthConfig::BearerToken {
            token: "  secret-token\n".into(),
        })
        .build()
        .unwrap();

    client.initialize().await.expect("initialize");
}

#[tokio::test]
async fn a_single_custom_header_reaches_the_wire() {
    let endpoint = serve(credential_gated_server(
        "custom-header-server",
        vec![("x-custom-token", "tok-xyz")],
    ))
    .await;

    let client = McpHttpClient::builder(endpoint)
        .timeout_secs(2)
        .auth(McpAuthConfig::Header {
            name: "X-Custom-Token".into(),
            value: "tok-xyz".into(),
        })
        .build()
        .unwrap();

    let initialized = client.initialize().await.expect("initialize");
    assert_eq!(initialized.server_info["name"], "custom-header-server");
}

#[tokio::test]
async fn every_header_of_a_multi_header_credential_reaches_the_wire() {
    // The server requires both, so a client that sent only the first fails.
    let endpoint = serve(credential_gated_server(
        "multi-header-server",
        vec![
            ("x-client-key", "ck-1"),
            ("authorization", "Bearer multi-secret"),
        ],
    ))
    .await;

    let client = McpHttpClient::builder(endpoint)
        .timeout_secs(2)
        .auth(McpAuthConfig::Headers {
            headers: vec![
                HttpHeader::new("X-Client-Key", "ck-1"),
                HttpHeader::new("Authorization", "Bearer multi-secret"),
            ],
        })
        .build()
        .unwrap();

    let initialized = client.initialize().await.expect("initialize");
    assert_eq!(initialized.server_info["name"], "multi-header-server");
}

#[tokio::test]
async fn basic_authentication_reaches_the_wire() {
    // `dXNlcjpwYXNz` is base64 of `user:pass`.
    let endpoint = serve(credential_gated_server(
        "basic-server",
        vec![("authorization", "Basic dXNlcjpwYXNz")],
    ))
    .await;

    let client = McpHttpClient::builder(endpoint)
        .timeout_secs(2)
        .auth(McpAuthConfig::Basic {
            username: "user".into(),
            password: "pass".into(),
        })
        .build()
        .unwrap();

    let initialized = client.initialize().await.expect("initialize");
    assert_eq!(initialized.server_info["name"], "basic-server");
}

#[tokio::test]
async fn a_401_becomes_a_typed_unauthorized_error() {
    // Callers decide on the variant, not on message text.
    let endpoint = serve(credential_gated_server(
        "never-reached",
        vec![("authorization", "Bearer the-right-one")],
    ))
    .await;

    let client = McpHttpClient::new(endpoint, 2).unwrap();
    let error = client.initialize().await.expect_err("a 401");

    assert!(error.is_unauthorized(), "{error:?}");
    assert!(
        !error.advertises_oauth(),
        "this server sent no challenge, so nothing advertised oauth"
    );
}

#[tokio::test]
async fn an_unauthorized_error_never_carries_the_raw_endpoint() {
    let endpoint = serve(credential_gated_server(
        "never-reached",
        vec![("authorization", "Bearer the-right-one")],
    ))
    .await;

    let client = McpHttpClient::builder(&endpoint)
        .timeout_secs(2)
        .auth(McpAuthConfig::QueryParam {
            name: "api_key".into(),
            value: "super-secret".into(),
        })
        .build()
        .unwrap();

    let error = client.initialize().await.expect_err("a 401");
    let rendered = error.to_string();

    assert!(
        !rendered.contains("super-secret"),
        "the query credential leaked into an error: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Authorization discovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discovery_reports_nothing_when_the_server_does_not_challenge() {
    let (endpoint, _) = spawn_test_server().await;
    let client = McpHttpClient::new(endpoint, 5).unwrap();

    assert!(client.discover_authorization().await.unwrap().is_none());
}

#[tokio::test]
async fn discovery_follows_the_challenge_to_both_metadata_documents() {
    let endpoint = spawn_discovery_server().await;
    let client = McpHttpClient::new(endpoint, 2).unwrap();

    let context = client
        .discover_authorization()
        .await
        .expect("discovery ran")
        .expect("the server challenged");

    assert_eq!(context.challenge.scheme, "Bearer");
    assert_eq!(context.challenge.realm.as_deref(), Some("mcp"));

    let resource = context
        .protected_resource_metadata
        .as_ref()
        .expect("protected-resource metadata");
    assert_eq!(resource.scopes_supported, ["mcp:tools"]);

    assert_eq!(context.authorization_server_metadata.len(), 1);
    assert_eq!(
        context.authorization_server_metadata[0]
            .authorization_endpoint
            .as_deref(),
        Some(format!("{}/authorize", resource.authorization_servers[0]).as_str())
    );
}

#[tokio::test]
async fn a_401_advertising_resource_metadata_is_flagged_as_oauth() {
    // This is what separates "sign in" from "paste a token" for a caller.
    let endpoint = spawn_discovery_server().await;
    let client = McpHttpClient::new(endpoint, 2).unwrap();

    let error = client.initialize().await.expect_err("a 401");

    assert!(error.is_unauthorized());
    assert!(error.advertises_oauth(), "{error:?}");
}

// ---------------------------------------------------------------------------
// Protocol negotiation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_server_negotiating_an_unknown_version_fails_the_handshake() {
    let app = Router::new().route(
        "/",
        post(|Json(body): Json<Value>| async move {
            Json(json!({
                "jsonrpc": "2.0",
                "id": body["id"].clone(),
                "result": {
                    "protocolVersion": "1999-01-01",
                    "capabilities": {},
                    "serverInfo": { "name": "ancient", "version": "1.0.0" },
                },
            }))
        }),
    );
    let client = McpHttpClient::new(serve(app).await, 2).unwrap();

    let error = client.initialize().await.expect_err("an unknown version");

    assert!(
        matches!(error, Error::UnsupportedProtocolVersion { ref version } if version == "1999-01-01"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_json_rpc_error_reply_becomes_an_rpc_error() {
    let app = Router::new().route(
        "/",
        post(|Json(body): Json<Value>| async move {
            Json(json!({
                "jsonrpc": "2.0",
                "id": body["id"].clone(),
                "error": { "code": -32601, "message": "method not found" },
            }))
        }),
    );
    let client = McpHttpClient::new(serve(app).await, 2).unwrap();

    let error = client.initialize().await.expect_err("an rpc error");

    assert!(matches!(error, Error::Rpc { .. }), "{error:?}");
    assert!(error.to_string().contains("method not found"));
}

#[tokio::test]
async fn a_reply_with_no_result_member_is_malformed() {
    let app = Router::new().route(
        "/",
        post(|Json(body): Json<Value>| async move {
            Json(json!({ "jsonrpc": "2.0", "id": body["id"].clone() }))
        }),
    );
    let client = McpHttpClient::new(serve(app).await, 2).unwrap();

    let error = client.initialize().await.expect_err("no result");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_non_json_body_is_malformed_rather_than_a_transport_failure() {
    let app = Router::new().route("/", post(|| async { "not json at all" }));
    let client = McpHttpClient::new(serve(app).await, 2).unwrap();

    let error = client.initialize().await.expect_err("not json");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_failure_status_becomes_an_http_error_carrying_the_body() {
    let app = Router::new().route(
        "/",
        post(|| async { (AxumStatus::INTERNAL_SERVER_ERROR, "the database is on fire") }),
    );
    let client = McpHttpClient::new(serve(app).await, 2).unwrap();

    let error = client.initialize().await.expect_err("a 500");

    match error {
        Error::Http { status, body, .. } => {
            assert_eq!(status, 500);
            assert_eq!(body, "the database is on fire");
        }
        other => panic!("expected an http error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_tool_reporting_failure_is_a_result_not_an_error() {
    // The call succeeded and the tool said no. Conflating the two would report
    // a network problem for a bad argument.
    let app = Router::new().route(
        "/",
        post(|Json(body): Json<Value>| async move {
            let rpc_method = body
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let result = if rpc_method == "initialize" {
                json!({
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "serverInfo": { "name": "failing", "version": "1.0.0" },
                })
            } else if rpc_method == "tools/list" {
                json!({ "tools": [] })
            } else {
                json!({
                    "isError": true,
                    "content": [{ "type": "text", "text": "city not found" }],
                })
            };
            Json(json!({ "jsonrpc": "2.0", "id": body["id"].clone(), "result": result }))
        }),
    );
    let client = McpHttpClient::new(serve(app).await, 2).unwrap();

    let result = client
        .call_tool("forecast", json!({}))
        .await
        .expect("the call itself succeeded");

    assert!(result.rendered.is_error);
    assert_eq!(result.rendered.text(), "city not found");
}

// ---------------------------------------------------------------------------
// Transport failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unreachable_endpoint_is_a_transport_error_with_a_redacted_endpoint() {
    // Port 1 on loopback refuses immediately, so this is fast and deterministic.
    let client = McpHttpClient::new("http://127.0.0.1:1/mcp?api_key=secret", 2).unwrap();

    let error = client.initialize().await.expect_err("connection refused");

    assert!(matches!(error, Error::Transport { .. }), "{error:?}");
    let rendered = error.to_string();
    assert!(!rendered.contains("secret"), "{rendered}");
    assert!(rendered.contains("http://127.0.0.1:1"), "{rendered}");
}

#[test]
fn a_malformed_proxy_url_does_not_fail_the_client_build() {
    // One bad proxy setting should not leave a host unable to reach anything.
    use tinymcp_bus::McpProxyConfig;

    let client = McpHttpClient::builder("https://example.test/mcp")
        .proxy(Some(McpProxyConfig {
            http_proxy: Some("not a url".into()),
            ..McpProxyConfig::default()
        }))
        .build();

    assert!(client.is_ok());
}

// ---------------------------------------------------------------------------
// SSE framing
// ---------------------------------------------------------------------------

#[test]
fn multiple_sse_frames_are_parsed_in_order() {
    let body = "id: 1\nevent: message\ndata: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
    let events = parse_sse_events(body).expect("events");

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id.as_deref(), Some("1"));
    assert_eq!(events[0].data.as_ref().unwrap()["a"], 1);
    assert_eq!(events[1].data.as_ref().unwrap()["b"], 2);
}

#[test]
fn a_half_received_frame_is_not_parsed() {
    // The data line has arrived but the terminating blank line has not. Parsing
    // it would mean decoding possibly-truncated JSON.
    assert!(
        first_complete_sse_data("event: message\ndata: {\"a\":1}\n")
            .expect("no error")
            .is_none()
    );
    assert!(
        first_complete_sse_data("event: mess")
            .expect("no error")
            .is_none()
    );
}

#[test]
fn the_first_terminated_frame_is_returned_even_with_more_bytes_behind_it() {
    // This is the behavior that stops a server holding its stream open from
    // stalling every call until the request timeout.
    let buffer = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\ndata: {\"b\":2}";

    let data = first_complete_sse_data(buffer)
        .expect("no error")
        .expect("the first complete frame");

    assert_eq!(data["result"]["ok"], true);
}

#[test]
fn keepalive_comments_and_dataless_events_are_skipped() {
    let buffer = ": keepalive\n\nevent: ping\n\ndata: {\"id\":7}\n\n";

    let data = first_complete_sse_data(buffer)
        .expect("no error")
        .expect("the data frame after the keepalive");

    assert_eq!(data["id"], 7);
}

#[test]
fn a_crlf_stream_splits_on_the_same_boundary() {
    let buffer = "event: message\r\ndata: {\"id\":9}\r\n\r\n";

    let data = first_complete_sse_data(buffer)
        .expect("no error")
        .expect("the crlf data frame");

    assert_eq!(data["id"], 9);
}

#[test]
fn a_multi_line_data_frame_is_joined_with_newlines() {
    // The event-stream format allows a payload to be split across `data:` lines.
    let buffer = "data: {\ndata: \"a\": 1\ndata: }\n\n";

    let data = first_complete_sse_data(buffer)
        .expect("no error")
        .expect("the joined frame");

    assert_eq!(data["a"], 1);
}

#[test]
fn a_frame_carrying_invalid_json_is_an_error() {
    let error = parse_sse_events("data: {not json}\n\n").expect_err("invalid json");
    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[test]
fn a_body_with_no_data_frame_yields_no_events_with_data() {
    let events = parse_sse_events(": just a keepalive\n\n").expect("no error");
    assert!(events.iter().all(|event| event.data.is_none()));
}

// ---------------------------------------------------------------------------
// Challenge parsing
// ---------------------------------------------------------------------------

#[test]
fn a_challenge_yields_its_scheme_realm_and_resource_metadata() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "WWW-Authenticate",
        HeaderValue::from_static(
            "Bearer realm=\"mcp\", resource_metadata=\"https://example.test/.well-known/oauth-protected-resource\"",
        ),
    );

    let challenge = parse_www_authenticate_challenge(&headers).expect("a challenge");

    assert_eq!(challenge.scheme, "Bearer");
    assert_eq!(challenge.realm.as_deref(), Some("mcp"));
    assert_eq!(
        challenge.resource_metadata.as_deref(),
        Some("https://example.test/.well-known/oauth-protected-resource")
    );
}

#[test]
fn a_challenge_with_no_attributes_still_yields_its_scheme() {
    let mut headers = HeaderMap::new();
    headers.insert("WWW-Authenticate", HeaderValue::from_static("Bearer"));

    let challenge = parse_www_authenticate_challenge(&headers).expect("a challenge");

    assert_eq!(challenge.scheme, "Bearer");
    assert_eq!(challenge.realm, None);
    assert_eq!(challenge.resource_metadata, None);
}

#[test]
fn a_response_with_no_challenge_yields_none() {
    assert!(parse_www_authenticate_challenge(&HeaderMap::new()).is_none());
}
