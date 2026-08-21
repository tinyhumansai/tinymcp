//! Unit tests for the browser OAuth flow.
//!
//! The pure parts — PKCE, token parsing, bundle round-tripping — are tested
//! directly. The rest runs against a loopback authorization server, because the
//! things worth asserting here are what actually goes on the wire: that the
//! verifier is sent with the exchange, that a client secret is kept when one is
//! issued, and that a refresh does not erase the user's other credentials.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::{Form, State};
use axum::http::StatusCode as AxumStatus;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::flow::OAuthFlow;
use super::tokens::{OAUTH_BUNDLE_KEY, refresh_if_expired};
use super::types::{AuthKind, TokenResponse};
use crate::Error;
use crate::registry::Store;
use tinymcp_bus::{CommandKind, InstalledServer, Transport};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A store holding one HTTP-remote install.
fn store_with_remote(url: &str) -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&InstalledServer {
            server_id: "srv-1".into(),
            qualified_name: "@test/remote".into(),
            display_name: "Remote".into(),
            description: None,
            icon_url: None,
            command_kind: CommandKind::Node,
            command: String::new(),
            args: Vec::new(),
            env_keys: Vec::new(),
            config: None,
            installed_at: 1_000,
            last_connected_at: None,
            transport: Transport::HttpRemote {
                url: url.to_string(),
            },
            enabled: true,
        })
        .unwrap();
    store
}

/// A store holding one subprocess install.
fn store_with_stdio() -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&InstalledServer {
            server_id: "srv-1".into(),
            qualified_name: "@test/local".into(),
            display_name: "Local".into(),
            description: None,
            icon_url: None,
            command_kind: CommandKind::Node,
            command: "npx".into(),
            args: Vec::new(),
            env_keys: Vec::new(),
            config: None,
            installed_at: 1_000,
            last_connected_at: None,
            transport: Transport::Stdio,
            enabled: true,
        })
        .unwrap();
    store
}

/// Binds a loopback port and serves `app`, returning its base URL.
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// What a token endpoint was asked for, so a test can assert on it.
#[derive(Clone, Default)]
struct TokenRequests {
    forms: Arc<parking_lot::Mutex<Vec<BTreeMap<String, String>>>>,
    count: Arc<AtomicUsize>,
}

/// A token endpoint that records what it was sent and answers with `reply`.
fn token_endpoint(state: TokenRequests, reply: Value) -> Router {
    Router::new()
        .route(
            "/token",
            post(
                move |State(state): State<TokenRequests>,
                      Form(form): Form<BTreeMap<String, String>>| {
                    let reply = reply.clone();
                    async move {
                        state.count.fetch_add(1, Ordering::SeqCst);
                        state.forms.lock().push(form);
                        Json(reply).into_response()
                    }
                },
            ),
        )
        .with_state(state)
}

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

#[test]
fn a_pkce_challenge_is_the_sha256_of_its_verifier() {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};

    let (verifier, challenge) = super::flow::generate_pkce_for_test();

    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(challenge, expected);
}

#[test]
fn a_pkce_verifier_is_within_the_length_the_specification_requires() {
    let (verifier, _) = super::flow::generate_pkce_for_test();
    assert!(
        (43..=128).contains(&verifier.len()),
        "{} characters",
        verifier.len()
    );
}

#[test]
fn two_pkce_pairs_differ() {
    // A reused verifier would let one authorization's code be exchanged
    // against another's challenge.
    let (first, _) = super::flow::generate_pkce_for_test();
    let (second, _) = super::flow::generate_pkce_for_test();
    assert_ne!(first, second);
}

// ---------------------------------------------------------------------------
// Token responses
// ---------------------------------------------------------------------------

#[test]
fn a_token_response_reads_every_field() {
    let parsed = TokenResponse::parse(&json!({
        "access_token": "a",
        "refresh_token": "r",
        "expires_in": 3600,
    }))
    .unwrap();

    assert_eq!(parsed.access_token, "a");
    assert_eq!(parsed.refresh_token.as_deref(), Some("r"));
    assert_eq!(parsed.expires_in, Some(3600));
}

#[test]
fn a_token_response_needs_only_an_access_token() {
    // Servers routinely omit the other two, and refusing their reply would
    // break sign-in for them.
    let parsed = TokenResponse::parse(&json!({ "access_token": "x" })).unwrap();

    assert_eq!(parsed.access_token, "x");
    assert_eq!(parsed.refresh_token, None);
    assert_eq!(parsed.expires_in, None);
}

#[test]
fn a_token_response_without_an_access_token_is_rejected() {
    let error =
        TokenResponse::parse(&json!({ "token_type": "bearer" })).expect_err("no access token");
    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_subprocess_install_needs_no_http_authorization() {
    let flow = OAuthFlow::new(None).unwrap();
    let detected = flow.detect(&store_with_stdio(), "srv-1").await.unwrap();

    assert_eq!(detected.kind, AuthKind::None);
}

#[tokio::test]
async fn a_store_failure_is_reported_rather_than_read_as_an_open_server() {
    // Reporting "needs nothing" for a lookup that failed would show the user a
    // state nobody checked.
    let flow = OAuthFlow::new(None).unwrap();
    let error = flow
        .detect(&Store::open_in_memory().unwrap(), "absent")
        .await
        .expect_err("no such install");

    assert!(matches!(error, Error::UnknownServer { .. }), "{error:?}");
}

#[tokio::test]
async fn a_server_that_does_not_challenge_is_reported_as_open() {
    let app = Router::new().route(
        "/",
        post(|Json(body): Json<Value>| async move {
            Json(json!({
                "jsonrpc": "2.0",
                "id": body["id"].clone(),
                "result": {
                    "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "serverInfo": { "name": "open", "version": "1" },
                },
            }))
        }),
    );
    let url = format!("{}/", serve(app).await);

    let flow = OAuthFlow::new(None).unwrap();
    let detected = flow
        .detect(&store_with_remote(&url), "srv-1")
        .await
        .unwrap();

    assert_eq!(detected.kind, AuthKind::None);
}

#[tokio::test]
async fn a_bare_401_is_reported_as_wanting_a_static_token() {
    // A challenge with no authorization server behind it is a plain bearer
    // gate, and a browser sign-in would have nowhere to go.
    let app = Router::new().route(
        "/",
        post(|| async {
            (
                AxumStatus::UNAUTHORIZED,
                [("WWW-Authenticate", "Bearer realm=\"mcp\"")],
                "",
            )
                .into_response()
        }),
    );
    let url = format!("{}/", serve(app).await);

    let flow = OAuthFlow::new(None).unwrap();
    let detected = flow
        .detect(&store_with_remote(&url), "srv-1")
        .await
        .unwrap();

    assert_eq!(detected.kind, AuthKind::Token);
    assert_eq!(detected.authorization_endpoint, None);
}

#[tokio::test]
async fn an_unreachable_server_is_reported_as_wanting_a_static_token() {
    // Better to let the user paste something and find out than to block them
    // behind a probe that could not complete.
    let flow = OAuthFlow::new(None).unwrap();
    let detected = flow
        .detect(&store_with_remote("http://127.0.0.1:1/mcp"), "srv-1")
        .await
        .unwrap();

    assert_eq!(detected.kind, AuthKind::Token);
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

/// Stores a bundle whose access token expired long ago.
fn store_expired_bundle(store: &Store, token_endpoint: &str, refresh_token: Option<&str>) {
    let bundle = json!({
        "refresh_token": refresh_token,
        "client_id": "cli-1",
        "client_secret": "sec-1",
        "token_endpoint": token_endpoint,
        "expires_at": 1,
    });
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([
                ("Authorization".to_string(), "Bearer stale".to_string()),
                (OAUTH_BUNDLE_KEY.to_string(), bundle.to_string()),
                ("X-Custom".to_string(), "kept".to_string()),
            ]),
        )
        .unwrap();
}

#[tokio::test]
async fn a_server_with_no_bundle_is_left_alone() {
    let store = store_with_remote("https://example.test/mcp");
    let http = reqwest::Client::new();

    assert!(!refresh_if_expired(&store, &http, "srv-1").await.unwrap());
}

#[tokio::test]
async fn a_token_that_is_still_valid_is_not_refreshed() {
    let store = store_with_remote("https://example.test/mcp");
    let far_future = super::tokens::now_unix() + 86_400;
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([(
                OAUTH_BUNDLE_KEY.to_string(),
                json!({
                    "refresh_token": "r",
                    "client_id": "cli-1",
                    "client_secret": null,
                    "token_endpoint": "http://127.0.0.1:1/token",
                    "expires_at": far_future,
                })
                .to_string(),
            )]),
        )
        .unwrap();

    // The endpoint is unroutable, so this would fail if a refresh were tried.
    assert!(
        !refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_bundle_with_no_refresh_token_cannot_refresh_and_says_so_quietly() {
    // The next call gets a 401, which prompts the user to sign in again. That
    // is the intended path, not an error here.
    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, "http://127.0.0.1:1/token", None);

    assert!(
        !refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn an_expired_token_is_refreshed_and_stored() {
    let requests = TokenRequests::default();
    let base = serve(token_endpoint(
        requests.clone(),
        json!({ "access_token": "fresh", "refresh_token": "r2", "expires_in": 3600 }),
    ))
    .await;

    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1"));

    assert!(
        refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
            .await
            .unwrap()
    );

    let env = store.load_env_values("srv-1").unwrap();
    assert_eq!(
        env.get("Authorization").map(String::as_str),
        Some("Bearer fresh")
    );
    assert_eq!(requests.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refresh_sends_the_grant_the_token_and_the_client_credentials() {
    let requests = TokenRequests::default();
    let base = serve(token_endpoint(
        requests.clone(),
        json!({ "access_token": "fresh" }),
    ))
    .await;

    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1"));

    refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
        .await
        .unwrap();

    let form = requests.forms.lock()[0].clone();
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(form.get("refresh_token").map(String::as_str), Some("r1"));
    assert_eq!(form.get("client_id").map(String::as_str), Some("cli-1"));
    assert_eq!(form.get("client_secret").map(String::as_str), Some("sec-1"));
}

#[tokio::test]
async fn a_refresh_keeps_the_existing_refresh_token_when_the_server_does_not_rotate_it() {
    // Dropping it would make this the last refresh possible.
    let base = serve(token_endpoint(
        TokenRequests::default(),
        json!({ "access_token": "fresh" }),
    ))
    .await;

    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1"));

    refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
        .await
        .unwrap();

    let env = store.load_env_values("srv-1").unwrap();
    let bundle: Value = serde_json::from_str(env.get(OAUTH_BUNDLE_KEY).expect("a bundle")).unwrap();
    assert_eq!(bundle["refresh_token"], json!("r1"));
}

#[tokio::test]
async fn a_refresh_does_not_erase_the_users_other_credentials() {
    // Storing is replace-all, so a refresh that started from an empty map would
    // silently drop every custom header the user configured.
    let base = serve(token_endpoint(
        TokenRequests::default(),
        json!({ "access_token": "fresh" }),
    ))
    .await;

    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1"));

    refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
        .await
        .unwrap();

    let env = store.load_env_values("srv-1").unwrap();
    assert_eq!(env.get("X-Custom").map(String::as_str), Some("kept"));
}

#[tokio::test]
async fn a_refresh_records_the_new_credential_names_on_the_server_row() {
    let base = serve(token_endpoint(
        TokenRequests::default(),
        json!({ "access_token": "fresh" }),
    ))
    .await;

    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1"));

    refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
        .await
        .unwrap();

    let names = store.get_server("srv-1").unwrap().env_keys;
    assert!(
        names.iter().any(|name| name == "Authorization"),
        "{names:?}"
    );
    assert!(
        names.iter().any(|name| name == OAUTH_BUNDLE_KEY),
        "{names:?}"
    );
}

#[tokio::test]
async fn a_corrupt_bundle_is_reported_rather_than_ignored() {
    let store = store_with_remote("https://example.test/mcp");
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([(OAUTH_BUNDLE_KEY.to_string(), "not json".to_string())]),
        )
        .unwrap();

    let error = refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
        .await
        .expect_err("a corrupt bundle");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_token_endpoint_failure_carries_the_body_that_explains_it() {
    // `invalid_grant` lives in the body, not the status. Discarding it would
    // leave a caller with a bare 400.
    let app = Router::new().route(
        "/token",
        post(|| async {
            (
                AxumStatus::BAD_REQUEST,
                Json(json!({ "error": "invalid_grant" })),
            )
        }),
    );
    let base = serve(app).await;

    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1"));

    let error = refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
        .await
        .expect_err("a rejected refresh");

    match error {
        Error::Http { status, body, .. } => {
            assert_eq!(status, 400);
            assert!(body.contains("invalid_grant"), "{body}");
        }
        other => panic!("expected an http error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The bundle key
// ---------------------------------------------------------------------------

#[test]
fn the_bundle_key_is_marked_internal() {
    // The two leading underscores are what stop it being sent as a request
    // header and shown in a credential list.
    assert!(OAUTH_BUNDLE_KEY.starts_with("__"), "{OAUTH_BUNDLE_KEY}");
}

// ---------------------------------------------------------------------------
// Pending state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn completing_with_an_unknown_state_is_refused() {
    let flow = OAuthFlow::new(None).unwrap();
    let error = flow
        .complete(&Store::open_in_memory().unwrap(), "never-issued", "code")
        .await
        .expect_err("an unknown state");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_fresh_flow_has_nothing_parked() {
    assert_eq!(OAuthFlow::new(None).unwrap().pending_count(), 0);
}
