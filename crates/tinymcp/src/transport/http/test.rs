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

// ---------------------------------------------------------------------------
// Header helpers
// ---------------------------------------------------------------------------
//
// Small, pure, and reached only on paths a live server does not exercise: a
// challenge that has to be parsed before discovery can start, a tool schema
// that asks for an argument to be mirrored into a header, and a configured
// header whose name cannot be encoded.

use super::headers::{
    apply_auth, header_to_string, mcp_param_headers_from_schema, parse_auth_attribute_list,
};
use reqwest::header::{HeaderMap as ReqwestHeaderMap, HeaderValue as ReqwestHeaderValue};

/// A header map holding one `WWW-Authenticate` value.
fn challenge_headers(value: &str) -> ReqwestHeaderMap {
    let mut headers = ReqwestHeaderMap::new();
    headers.insert(
        "WWW-Authenticate",
        ReqwestHeaderValue::from_str(value).expect("a valid header value"),
    );
    headers
}

#[test]
fn a_challenge_is_read_down_to_its_scheme_and_attributes() {
    let challenge = parse_www_authenticate_challenge(&challenge_headers(
        "Bearer realm=\"mcp\", resource_metadata=\"https://example.test/.well-known/x\"",
    ))
    .expect("a challenge");

    assert_eq!(challenge.scheme, "Bearer");
    assert_eq!(challenge.realm.as_deref(), Some("mcp"));
    assert_eq!(
        challenge.resource_metadata.as_deref(),
        Some("https://example.test/.well-known/x")
    );
}

#[test]
fn a_challenge_with_no_attributes_is_still_a_challenge() {
    // A bare `Bearer` is what a server that wants a static token sends. Reading
    // it as nothing would lose the only signal that authentication is wanted.
    let challenge =
        parse_www_authenticate_challenge(&challenge_headers("Bearer")).expect("a challenge");

    assert_eq!(challenge.scheme, "Bearer");
    assert_eq!(challenge.resource_metadata, None);
}

#[test]
fn no_challenge_header_reads_as_no_challenge() {
    assert!(parse_www_authenticate_challenge(&ReqwestHeaderMap::new()).is_none());
}

#[test]
fn an_attribute_list_tolerates_spacing_and_quoting() {
    let attributes =
        parse_auth_attribute_list("realm=\"mcp\" ,  resource_metadata=https://example.test");

    assert_eq!(attributes.get("realm").map(String::as_str), Some("mcp"));
    assert_eq!(
        attributes.get("resource_metadata").map(String::as_str),
        Some("https://example.test")
    );
}

#[test]
fn a_header_that_is_not_present_reads_as_none() {
    assert_eq!(
        header_to_string(&ReqwestHeaderMap::new(), "Mcp-Session-Id"),
        None
    );
}

#[test]
fn a_header_that_is_present_reads_as_its_text() {
    let mut headers = ReqwestHeaderMap::new();
    headers.insert("Mcp-Session-Id", ReqwestHeaderValue::from_static("s-1"));

    assert_eq!(
        header_to_string(&headers, "Mcp-Session-Id").as_deref(),
        Some("s-1")
    );
}

/// A remote tool whose `tenant` property asks to be mirrored into a header.
fn tool_with_header_property() -> tinymcp_bus::McpRemoteTool {
    serde_json::from_value(json!({
        "name": "needs_header",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tenant": { "type": "string", "x-mcp-header": "tenant" },
                "plain": { "type": "string" },
            },
        },
    }))
    .expect("a tool decodes")
}

#[test]
fn an_argument_marked_for_mirroring_becomes_a_header() {
    let headers =
        mcp_param_headers_from_schema(&tool_with_header_property(), &json!({ "tenant": "acme" }))
            .expect("headers build");

    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].0.as_str(), "mcp-param-tenant");
    assert_eq!(headers[0].1.to_str().unwrap(), "acme");
}

#[test]
fn an_argument_not_marked_for_mirroring_stays_in_the_body() {
    let headers =
        mcp_param_headers_from_schema(&tool_with_header_property(), &json!({ "plain": "value" }))
            .expect("headers build");

    assert!(headers.is_empty());
}

#[test]
fn a_marked_argument_that_was_not_supplied_produces_no_header() {
    let headers = mcp_param_headers_from_schema(&tool_with_header_property(), &json!({}))
        .expect("headers build");

    assert!(headers.is_empty());
}

#[test]
fn a_non_string_argument_is_mirrored_as_its_json_form() {
    let headers =
        mcp_param_headers_from_schema(&tool_with_header_property(), &json!({ "tenant": 42 }))
            .expect("headers build");

    assert_eq!(headers[0].1.to_str().unwrap(), "42");
}

#[test]
fn arguments_that_are_not_an_object_produce_no_headers() {
    let headers =
        mcp_param_headers_from_schema(&tool_with_header_property(), &json!("not an object"))
            .expect("headers build");

    assert!(headers.is_empty());
}

#[test]
fn a_tool_with_no_properties_produces_no_headers() {
    let tool: tinymcp_bus::McpRemoteTool = serde_json::from_value(json!({
        "name": "plain",
        "inputSchema": { "type": "object" },
    }))
    .unwrap();

    assert!(
        mcp_param_headers_from_schema(&tool, &json!({ "anything": 1 }))
            .expect("headers build")
            .is_empty()
    );
}

#[test]
fn an_argument_value_that_cannot_be_a_header_is_refused() {
    // A newline in a header value is request splitting. Refusing is the only
    // safe answer, and it has to name the property so the user can fix it.
    let error = mcp_param_headers_from_schema(
        &tool_with_header_property(),
        &json!({ "tenant": "acme\r\nX-Injected: yes" }),
    )
    .expect_err("an unusable header value");

    assert!(error.to_string().contains("tenant"), "{error}");
}

#[test]
fn a_schema_asking_for_an_unusable_header_name_is_refused() {
    let tool: tinymcp_bus::McpRemoteTool = serde_json::from_value(json!({
        "name": "bad",
        "inputSchema": {
            "type": "object",
            "properties": { "tenant": { "x-mcp-header": "not a header name" } },
        },
    }))
    .unwrap();

    assert!(mcp_param_headers_from_schema(&tool, &json!({ "tenant": "acme" })).is_err());
}

#[tokio::test]
async fn a_configured_header_that_cannot_be_encoded_is_skipped_rather_than_fatal() {
    // One unusable entry in a user's configuration must not take down every
    // request to that server; the rest of the credentials still apply.
    let client = reqwest::Client::new();
    let request = client.get("http://127.0.0.1:1/");

    let auth = McpAuthConfig::Headers {
        headers: vec![
            HttpHeader {
                name: "not a header name".into(),
                value: "whatever".into(),
            },
            HttpHeader {
                name: "X-Fine".into(),
                value: "ok".into(),
            },
        ],
    };

    let built = apply_auth(request, &auth)
        .build()
        .expect("the request builds");

    assert!(built.headers().get("X-Fine").is_some());
    assert_eq!(built.headers().len(), 1);
}

// ---------------------------------------------------------------------------
// Reading an SSE body
// ---------------------------------------------------------------------------

use super::sse::parse_sse_message;

#[test]
fn a_body_with_no_data_frame_is_reported_rather_than_read_as_empty() {
    // A response made only of keepalive comments carries no reply, and
    // returning nothing would look like a server that answered with null.
    let error = parse_sse_message(": keepalive\n\n").expect_err("no data frame");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[test]
fn a_data_frame_is_read_out_of_a_body_that_also_carries_comments() {
    let value = parse_sse_message(": keepalive\n\nevent: message\ndata: {\"ok\":true}\n\n")
        .expect("a data frame");

    assert_eq!(value, json!({ "ok": true }));
}

#[test]
fn a_frame_whose_payload_is_not_json_is_reported() {
    assert!(parse_sse_message("data: not json\n\n").is_err());
}

#[test]
fn an_unterminated_buffer_reads_as_nothing_yet_rather_than_as_truncated_json() {
    // The half-received line is the whole reason this function exists: decoding
    // it would produce a parse error for a response that is simply still
    // arriving.
    assert_eq!(
        first_complete_sse_data("data: {\"ok\":tr").expect("still arriving"),
        None
    );
}

#[test]
fn a_terminated_frame_is_read_even_when_more_follows_it() {
    let value = first_complete_sse_data("data: {\"ok\":true}\n\ndata: {\"part")
        .expect("a complete frame")
        .expect("some data");

    assert_eq!(value, json!({ "ok": true }));
}

#[test]
fn a_crlf_stream_splits_on_the_same_boundary_as_an_lf_one() {
    let value = first_complete_sse_data("data: {\"ok\":true}\r\n\r\n")
        .expect("a complete frame")
        .expect("some data");

    assert_eq!(value, json!({ "ok": true }));
}

#[test]
fn a_terminated_frame_carrying_only_a_comment_means_keep_reading() {
    // `None` here is "nothing yet", not "give up": a keepalive is exactly what
    // a server sends while it is still working.
    assert_eq!(
        first_complete_sse_data(": keepalive\n\n").expect("a complete frame"),
        None
    );
}

#[test]
fn a_final_event_with_no_trailing_blank_line_is_still_an_event() {
    // Servers do send this when they close immediately after replying.
    let value = parse_sse_message("data: {\"ok\":true}").expect("a data frame");

    assert_eq!(value, json!({ "ok": true }));
}

#[test]
fn a_payload_split_over_several_data_lines_is_joined() {
    // The SSE spec concatenates them with newlines, which JSON tolerates.
    let value = parse_sse_message("data: {\"ok\":\ndata: true}\n\n").expect("a data frame");

    assert_eq!(value, json!({ "ok": true }));
}

// ---------------------------------------------------------------------------
// Redirect policy
// ---------------------------------------------------------------------------

#[test]
fn follows_an_https_to_https_redirect() {
    use super::{RedirectDecision, redirect_decision};
    assert_eq!(
        redirect_decision(Some("https"), "https", 1),
        RedirectDecision::Follow
    );
}

#[test]
fn refuses_an_https_to_http_downgrade() {
    use super::{RedirectDecision, redirect_decision};
    // A same-origin downgrade is the gap reqwest leaves open: a bearer on a
    // same-host hop, and any custom header or query-param credential on any
    // hop, are not stripped. The policy refuses the hop instead.
    let decision = redirect_decision(Some("https"), "http", 1);
    assert!(matches!(decision, RedirectDecision::Error(_)));
}

#[test]
fn does_not_refuse_a_plain_http_redirect_that_started_on_http() {
    // An endpoint the host already allowed over HTTP (loopback, or an
    // unauthenticated server) is not downgraded by staying on HTTP.
    use super::{RedirectDecision, redirect_decision};
    assert_eq!(
        redirect_decision(Some("http"), "http", 1),
        RedirectDecision::Follow
    );
}

#[test]
fn caps_the_redirect_chain_at_max_redirects() {
    // `hops` counts the initial URL (reqwest's accounting), so the cap fires
    // one past `MAX_REDIRECTS` — matching `Policy::limited`.
    use super::{MAX_REDIRECTS, RedirectDecision, redirect_decision};
    assert_eq!(
        redirect_decision(Some("https"), "https", MAX_REDIRECTS),
        RedirectDecision::Follow
    );
    assert!(matches!(
        redirect_decision(Some("https"), "https", MAX_REDIRECTS + 1),
        RedirectDecision::Error(_)
    ));
}
