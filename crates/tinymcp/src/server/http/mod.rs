//! The Streamable HTTP + SSE transport for an MCP server.
//!
//! One route, `/`, three verbs:
//!
//! - `POST` carries JSON-RPC. `initialize` needs no session and mints one,
//!   returned in [`HEADER_SESSION_ID`]; every other request must name a live
//!   session and repeat the protocol version it negotiated in
//!   [`HEADER_PROTOCOL_VERSION`].
//! - `GET` opens the session's server-sent event stream.
//! - `DELETE` ends the session.
//!
//! The header names are the `tinymcp_bus` constants this crate's own client
//! sends, so the two cannot drift. With an `auth_token` configured, every
//! request must carry `Authorization: Bearer <token>` or is refused before
//! anything else is looked at.
//!
//! Transport headers reach the handler through [`RequestContext::header`]:
//! a host that propagates state across the hop (a delegation depth, a trace
//! id) reads it there rather than this transport knowing about it.
//!
//! Session ids are bearer-like values, so they are logged only as a short
//! SHA-256 prefix.
//!
//! [`RequestContext::header`]: crate::server::RequestContext::header

use crate::RequestHeadersExt;
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::State,
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::post,
};
use parking_lot::Mutex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tinymcp_bus::{HEADER_PROTOCOL_VERSION, HEADER_SESSION_ID, LATEST_PROTOCOL_VERSION};
use tokio::sync::{broadcast, oneshot};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use uuid::Uuid;

use super::{ClientSession, McpServerHandler, RequestHeaders, handle_value};
use crate::error::{Error, Result};

/// Where and how the HTTP server listens.
#[derive(Debug, Clone)]
pub struct HttpServerConfig {
    /// The address to bind. Port `0` picks a free one; see
    /// [`run_http_reporting`] to learn which.
    pub bind_addr: SocketAddr,
    /// When set, every request must carry `Authorization: Bearer <token>`.
    pub auth_token: Option<String>,
}

/// Serves MCP over Streamable HTTP until the server stops.
///
/// # Errors
///
/// [`Error::ServerBind`] when the address cannot be bound, and
/// [`Error::ServerIo`] when the listener fails.
pub async fn run_http(handler: Arc<dyn McpServerHandler>, config: HttpServerConfig) -> Result<()> {
    run_http_reporting(handler, config, None).await
}

/// Like [`run_http`], but sends the address actually bound through `ready`
/// once the listener is up — how a caller binding port `0` learns the port.
///
/// # Errors
///
/// As [`run_http`].
pub async fn run_http_reporting(
    handler: Arc<dyn McpServerHandler>,
    config: HttpServerConfig,
    ready: Option<oneshot::Sender<SocketAddr>>,
) -> Result<()> {
    let state = AppState::new(handler, config.auth_token);
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .map_err(|source| Error::ServerBind {
            addr: config.bind_addr,
            source: Box::new(source),
        })?;
    let local_addr = listener.local_addr().map_err(Error::server_io)?;
    tracing::info!("[mcp_server] HTTP/SSE listening on http://{local_addr}");
    if let Some(tx) = ready {
        // A caller that stopped waiting has nothing to be told.
        let _ = tx.send(local_addr);
    }

    axum::serve(listener, router(state))
        .await
        .map_err(Error::server_io)
}

/// What the server remembers about one session.
#[derive(Debug)]
struct SessionRecord {
    protocol_version: String,
}

/// One server-sent event addressed to one session.
#[derive(Debug, Clone)]
pub(crate) struct SessionEvent {
    pub(crate) session_id: String,
    pub(crate) event: Option<String>,
    pub(crate) data: String,
}

#[derive(Clone)]
pub(crate) struct AppState {
    handler: Arc<dyn McpServerHandler>,
    sessions: Arc<Mutex<HashMap<String, SessionRecord>>>,
    auth_token: Option<String>,
    pub(crate) event_tx: broadcast::Sender<SessionEvent>,
}

impl AppState {
    pub(crate) fn new(handler: Arc<dyn McpServerHandler>, auth_token: Option<String>) -> Self {
        let (event_tx, _) = broadcast::channel(128);
        Self {
            handler,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            auth_token,
            event_tx,
        }
    }

    /// A fresh session for one request. Each POST is dispatched on its own:
    /// the transport keeps only the negotiated version per session id.
    fn session(&self) -> ClientSession {
        ClientSession::new(self.handler.source_type_prefix())
    }
}

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/", post(handle_post).get(handle_get).delete(handle_delete))
        .with_state(state)
}

async fn handle_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Some(response) = check_auth(&state, &headers) {
        return response;
    }

    let session_id = header_value(&headers, HEADER_SESSION_ID);
    let protocol_version = header_value(&headers, HEADER_PROTOCOL_VERSION);
    let rpc_method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let redacted_session_id = session_id.map(redact_session_id);

    tracing::debug!(
        "[mcp_server] HTTP POST method={rpc_method} session={:?} protocol={:?}",
        redacted_session_id.as_deref(),
        protocol_version
    );

    if rpc_method == "initialize" {
        return handle_initialize(&state, &headers, body).await;
    }

    let Some(session_id) = session_id else {
        log_request_rejected("missing/invalid session", None, protocol_version, None);
        return text_error(
            StatusCode::BAD_REQUEST,
            "missing or invalid Mcp-Session-Id header",
        );
    };

    if let Some(response) = reject_session(&state, session_id, protocol_version) {
        return response;
    }

    // A tool call can run arbitrary host work, so its future can be very
    // large. Box it onto the heap so this handler's frame stays small — an
    // inline giant future here overflows the worker stack.
    let has_id = body.get("id").is_some();
    let request_headers = request_headers(&headers);
    let mut session = state.session();
    let mut responses = Box::pin(handle_value(
        &*state.handler,
        &mut session,
        &request_headers,
        body,
    ))
    .await;
    if !has_id {
        return StatusCode::NO_CONTENT.into_response();
    }

    match responses.len() {
        0 => StatusCode::NO_CONTENT.into_response(),
        1 => Json(responses.remove(0)).into_response(),
        _ => Json(Value::Array(responses)).into_response(),
    }
}

async fn handle_initialize(state: &AppState, headers: &HeaderMap, body: Value) -> Response {
    let request_headers = request_headers(headers);
    let mut session = state.session();
    // Boxed for the reason given in `handle_post`.
    let responses = Box::pin(handle_value(
        &*state.handler,
        &mut session,
        &request_headers,
        body,
    ))
    .await;
    let Some(response) = responses.into_iter().next() else {
        return StatusCode::NO_CONTENT.into_response();
    };

    if response.get("error").is_some() {
        return Json(response).into_response();
    }

    let negotiated = response
        .get("result")
        .and_then(|result| result.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(LATEST_PROTOCOL_VERSION)
        .to_string();

    let session_id = Uuid::new_v4().to_string();
    let redacted_session_id = redact_session_id(&session_id);
    tracing::debug!(
        "[mcp_server] HTTP session created id={redacted_session_id} protocol={negotiated}"
    );
    state.sessions.lock().insert(
        session_id.clone(),
        SessionRecord {
            protocol_version: negotiated,
        },
    );

    ([(HEADER_SESSION_ID, session_id.as_str())], Json(response)).into_response()
}

async fn handle_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = check_auth(&state, &headers) {
        return response;
    }

    let protocol_version = header_value(&headers, HEADER_PROTOCOL_VERSION);
    let Some(session_id) = header_value(&headers, HEADER_SESSION_ID) else {
        log_request_rejected("missing/invalid session", None, protocol_version, None);
        return text_error(StatusCode::BAD_REQUEST, "missing Mcp-Session-Id header");
    };

    if let Some(response) = reject_session(&state, session_id, protocol_version) {
        return response;
    }

    let redacted_session_id = redact_session_id(session_id);
    tracing::debug!("[mcp_server] HTTP events stream opened session={redacted_session_id}");

    let session_id = session_id.to_string();
    let stream = BroadcastStream::new(state.event_tx.subscribe()).filter_map(move |message| {
        let event = match message {
            Ok(event) if event.session_id == session_id => event,
            _ => return None,
        };
        let mut sse_event = Event::default().data(event.data);
        if let Some(name) = event.event {
            sse_event = sse_event.event(name);
        }
        Some(Ok::<Event, Infallible>(sse_event))
    });

    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(10))
                .text("keepalive"),
        )
        .into_response()
}

async fn handle_delete(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = check_auth(&state, &headers) {
        return response;
    }

    let Some(session_id) = header_value(&headers, HEADER_SESSION_ID) else {
        log_request_rejected(
            "missing/invalid session",
            None,
            header_value(&headers, HEADER_PROTOCOL_VERSION),
            None,
        );
        return text_error(StatusCode::BAD_REQUEST, "missing Mcp-Session-Id header");
    };

    if state.sessions.lock().remove(session_id).is_some() {
        let redacted_session_id = redact_session_id(session_id);
        tracing::debug!("[mcp_server] HTTP session closed id={redacted_session_id}");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// The rejection to send unless `session_id` is live and `protocol_version`
/// is the one it negotiated.
fn reject_session(
    state: &AppState,
    session_id: &str,
    protocol_version: Option<&str>,
) -> Option<Response> {
    let expected_protocol = {
        let sessions = state.sessions.lock();
        let Some(record) = sessions.get(session_id) else {
            log_request_rejected(
                "unknown/expired session",
                Some(session_id),
                protocol_version,
                None,
            );
            return Some(text_error(
                StatusCode::NOT_FOUND,
                "unknown or expired MCP session",
            ));
        };
        record.protocol_version.clone()
    };

    if protocol_version != Some(expected_protocol.as_str()) {
        log_request_rejected(
            "protocol mismatch",
            Some(session_id),
            protocol_version,
            Some(expected_protocol.as_str()),
        );
        return Some(text_error(
            StatusCode::BAD_REQUEST,
            "missing or invalid MCP-Protocol-Version header",
        ));
    }
    None
}

fn check_auth(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let expected = state.auth_token.as_deref()?;
    let provided = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    if provided == Some(expected) {
        return None;
    }
    tracing::debug!("[mcp_server] HTTP request rejected: bearer auth mismatch");
    Some(
        (
            StatusCode::UNAUTHORIZED,
            [(CONTENT_TYPE.as_str(), "text/plain")],
            "unauthorized",
        )
            .into_response(),
    )
}

/// The request's headers for the handler: the first value of each, as
/// [`HeaderMap::get`] reads it, skipping values that are not text.
fn request_headers(headers: &HeaderMap) -> RequestHeaders {
    let mut out = RequestHeaders::new();
    for name in headers.keys() {
        if let Some(value) = header_value(headers, name.as_str()) {
            out.insert(name.as_str(), value);
        }
    }
    out
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn redact_session_id(session_id: &str) -> String {
    let digest = Sha256::digest(session_id.as_bytes());
    let mut redacted = String::from("sha256:");
    for byte in digest.iter().take(4) {
        // Writing to a `String` cannot fail.
        let _ = write!(redacted, "{byte:02x}");
    }
    redacted
}

fn log_request_rejected(
    reason: &str,
    session_id: Option<&str>,
    protocol_version: Option<&str>,
    expected_protocol: Option<&str>,
) {
    let redacted_session_id = session_id.map(redact_session_id);
    tracing::debug!(
        "[mcp_server] HTTP request rejected reason={reason} session={:?} protocol={:?} expected_protocol={:?}",
        redacted_session_id.as_deref(),
        protocol_version,
        expected_protocol
    );
}

fn text_error(status: StatusCode, message: &str) -> Response {
    (status, message.to_string()).into_response()
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
