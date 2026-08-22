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

// ---------------------------------------------------------------------------
// The flow against a loopback authorization server
// ---------------------------------------------------------------------------
//
// Everything above works on the pieces. What follows drives the whole
// authorization: the 401 that starts it, the two discovery documents, dynamic
// client registration, the authorize URL that gets built, and the code
// exchange that ends it. None of that exists without something answering.

use axum::routing::get;

/// How the loopback authorization server should behave.
#[derive(Debug, Default)]
struct Authority {
    registrations: AtomicUsize,
    exchanges: AtomicUsize,
    /// The form the token endpoint last received.
    last_form: parking_lot::Mutex<Option<String>>,
    /// Leave the registration endpoint out of the metadata.
    without_registration: std::sync::atomic::AtomicBool,
    /// Advertise only the implicit grant.
    without_authorization_code: std::sync::atomic::AtomicBool,
    /// Refuse the code exchange.
    refuse_exchange: std::sync::atomic::AtomicBool,
}

/// The registration and token endpoints, split out to keep the router builder
/// above readable rather than one expression the length of the function.
fn endpoints() -> Router<Arc<Authority>> {
    Router::new()
        .route(
            "/register",
            post(|State(state): State<Arc<Authority>>| async move {
                state.registrations.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "client_id": "client-1",
                    "client_secret": "client-secret-1",
                }))
            }),
        )
        .route(
            "/token",
            post(
                |State(state): State<Arc<Authority>>, body: String| async move {
                    state.exchanges.fetch_add(1, Ordering::SeqCst);
                    *state.last_form.lock() = Some(body);

                    if state.refuse_exchange.load(Ordering::SeqCst) {
                        return (
                            AxumStatus::BAD_REQUEST,
                            "{\"error\":\"invalid_grant\"}".to_string(),
                        )
                            .into_response();
                    }

                    axum::Json(json!({
                        "access_token": "at-1",
                        "refresh_token": "rt-1",
                        "token_type": "Bearer",
                        "expires_in": 3600,
                    }))
                    .into_response()
                },
            ),
        )
}

/// An MCP server that demands OAuth, together with the authorization server it
/// points at. Both live on one origin, which is legal and keeps the test short.
async fn authority() -> (String, Arc<Authority>) {
    let state = Arc::new(Authority::default());

    // The origin is not known until the listener binds, so the challenge is
    // rewritten by a layer once it is.
    let origin: Arc<parking_lot::Mutex<String>> = Arc::new(parking_lot::Mutex::new(String::new()));

    let app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get({
                let origin = Arc::clone(&origin);
                move || {
                    let origin = origin.lock().clone();
                    async move {
                        axum::Json(json!({
                            "resource": format!("{origin}/mcp"),
                            "authorization_servers": [origin],
                        }))
                    }
                }
            }),
        )
        // Absent on purpose: the client tries this first and must fall back.
        .route(
            "/.well-known/openid-configuration",
            get(|| async { AxumStatus::NOT_FOUND }),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get({
                let origin = Arc::clone(&origin);
                let state = Arc::clone(&state);
                move || {
                    let origin = origin.lock().clone();
                    let state = Arc::clone(&state);
                    async move {
                        let mut metadata = json!({
                            "issuer": origin,
                            "authorization_endpoint": format!("{origin}/authorize"),
                            "token_endpoint": format!("{origin}/token"),
                            "registration_endpoint": format!("{origin}/register"),
                            "grant_types_supported": ["authorization_code", "refresh_token"],
                        });
                        if state.without_registration.load(Ordering::SeqCst) {
                            metadata
                                .as_object_mut()
                                .unwrap()
                                .remove("registration_endpoint");
                        }
                        if state.without_authorization_code.load(Ordering::SeqCst) {
                            metadata["grant_types_supported"] = json!(["implicit"]);
                        }
                        axum::Json(metadata)
                    }
                }
            }),
        )
        .merge(endpoints())
        .with_state(Arc::clone(&state));

    let base = serve(app).await;
    *origin.lock() = base.clone();

    (base, state)
}

/// The 401 challenge has to name the bound origin, which is only known after
/// the listener binds. Serving it from a second router keeps the first simple.
async fn authority_with_challenge() -> (String, Arc<Authority>) {
    let (base, state) = authority().await;

    // Re-serve the MCP endpoint with the origin baked into the challenge.
    let challenge =
        format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource\"");
    // A bare handler rather than `get(..)`: the transport POSTs to the MCP
    // endpoint, and a GET-only fallback would answer 405 instead of the 401
    // that starts discovery.
    let app = Router::new().fallback(move || {
        let challenge = challenge.clone();
        async move {
            (
                AxumStatus::UNAUTHORIZED,
                [("WWW-Authenticate", challenge)],
                "unauthorized",
            )
                .into_response()
        }
    });
    let mcp_base = serve(app).await;

    (format!("{mcp_base}/mcp"), state)
}

/// One query parameter off an authorize URL.
fn authorize_param(url: &str, name: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// The flow under test.
fn flow() -> OAuthFlow {
    OAuthFlow::new(None).expect("the flow builds")
}

// ---------------------------------------------------------------------------
// Beginning
// ---------------------------------------------------------------------------

#[tokio::test]
async fn beginning_a_sign_in_registers_a_client_and_returns_an_authorize_url() {
    let (endpoint, state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);

    let url = flow()
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .expect("begin");

    assert_eq!(state.registrations.load(Ordering::SeqCst), 1);
    assert!(url.contains("/authorize"), "{url}");
}

#[tokio::test]
async fn the_authorize_url_carries_the_pkce_challenge_and_its_method() {
    // Without PKCE an intercepted code is enough to mint a token. The method
    // matters as much as the challenge: `plain` would make the verifier
    // pointless.
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);

    let url = flow()
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();

    assert_eq!(
        authorize_param(&url, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert!(authorize_param(&url, "code_challenge").is_some_and(|challenge| !challenge.is_empty()));
}

#[tokio::test]
async fn the_authorize_url_carries_the_state_the_callback_will_return() {
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);

    let url = flow()
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();

    assert!(authorize_param(&url, "state").is_some_and(|state| !state.is_empty()));
}

#[tokio::test]
async fn the_authorize_url_names_the_resource_being_authorized() {
    // A token minted for one resource must not be usable at another, and the
    // authorization server can only scope it if it is told.
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);

    let url = flow()
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();

    assert_eq!(
        authorize_param(&url, "resource").as_deref(),
        Some(&*endpoint)
    );
}

#[tokio::test]
async fn the_authorize_url_carries_the_redirect_the_host_bound() {
    // Only the host knows which port it actually bound.
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);

    let url = flow()
        .begin(&store, "srv-1", "http://127.0.0.1:9999/callback")
        .await
        .unwrap();

    assert_eq!(
        authorize_param(&url, "redirect_uri").as_deref(),
        Some("http://127.0.0.1:9999/callback")
    );
}

#[tokio::test]
async fn beginning_parks_exactly_one_pending_authorization() {
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);
    let flow = flow();

    flow.begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();

    assert_eq!(flow.pending_count(), 1);
}

#[tokio::test]
async fn discovery_falls_back_from_the_openid_document_to_the_oauth_one() {
    // Servers publish one or the other and rarely say which. The loopback
    // authority answers 404 on the OpenID document, so reaching an authorize
    // URL at all proves the fallback ran.
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);

    assert!(
        flow()
            .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
            .await
            .is_ok()
    );
}

// ---------------------------------------------------------------------------
// Refusing to begin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stdio_server_cannot_be_signed_in_to() {
    // OAuth is a property of an HTTP endpoint; a subprocess has none.
    let error = flow()
        .begin(
            &store_with_stdio(),
            "srv-1",
            "http://127.0.0.1:7788/callback",
        )
        .await
        .expect_err("stdio");

    assert!(error.to_string().contains("http-remote"), "{error}");
}

#[tokio::test]
async fn an_authorization_server_without_dynamic_registration_is_refused() {
    // There is no way to obtain a client id without it, and picking a server
    // that cannot finish the flow would fail later and less clearly.
    let (endpoint, state) = authority_with_challenge().await;
    state.without_registration.store(true, Ordering::SeqCst);
    let store = store_with_remote(&endpoint);

    let error = flow()
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .expect_err("no registration endpoint");

    assert!(matches!(error, Error::AuthDiscovery { .. }), "{error:?}");
}

#[tokio::test]
async fn an_authorization_server_not_offering_the_authorization_code_grant_is_refused() {
    let (endpoint, state) = authority_with_challenge().await;
    state
        .without_authorization_code
        .store(true, Ordering::SeqCst);
    let store = store_with_remote(&endpoint);

    let error = flow()
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .expect_err("no authorization_code grant");

    assert!(matches!(error, Error::AuthDiscovery { .. }), "{error:?}");
}

#[tokio::test]
async fn a_server_that_does_not_want_authorization_is_refused() {
    // Beginning a sign-in against a server that never asked for one would
    // send the user to an authorization page for nothing.
    let app = Router::new()
        .fallback(|| async { Json(json!({ "jsonrpc": "2.0", "id": 1, "result": {} })) });
    let base = serve(app).await;
    let store = store_with_remote(&base);

    let error = flow()
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .expect_err("no challenge");

    assert!(
        error.to_string().contains("does not require authorization"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// Completing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn completing_exchanges_the_code_and_reports_the_server_to_reconnect() {
    // Reconnecting is the caller's: this flow has no connection map, and the
    // caller does.
    let (endpoint, state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);
    let flow = flow();
    let url = flow
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();
    let returned_state = authorize_param(&url, "state").unwrap();

    let server_id = flow
        .complete(&store, &returned_state, "the-code")
        .await
        .expect("complete");

    assert_eq!(server_id, "srv-1");
    assert_eq!(state.exchanges.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn completing_stores_the_token_as_an_ordinary_authorization_header() {
    // Storing it as a header means the connect path needs no special case for
    // an OAuth server: it builds headers from stored credentials either way.
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);
    let flow = flow();
    let url = flow
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();
    let returned_state = authorize_param(&url, "state").unwrap();

    flow.complete(&store, &returned_state, "the-code")
        .await
        .unwrap();

    let env = store.load_env_values("srv-1").unwrap();
    assert_eq!(
        env.get("Authorization").map(String::as_str),
        Some("Bearer at-1")
    );
}

#[tokio::test]
async fn the_exchange_sends_the_verifier_that_matches_the_challenge() {
    let (endpoint, state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);
    let flow = flow();
    let url = flow
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();
    let returned_state = authorize_param(&url, "state").unwrap();

    flow.complete(&store, &returned_state, "the-code")
        .await
        .unwrap();

    let form = state.last_form.lock().clone().expect("the token form");
    assert!(form.contains("code_verifier="), "{form}");
    assert!(form.contains("grant_type=authorization_code"), "{form}");
}

#[tokio::test]
async fn the_stored_bundle_keeps_the_refresh_token_out_of_the_credential_listing() {
    // The bundle key is marked internal, so the connect path never sends it to
    // a server and a credential listing does not show it to the user.
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);
    let flow = flow();
    let url = flow
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();
    let returned_state = authorize_param(&url, "state").unwrap();

    flow.complete(&store, &returned_state, "the-code")
        .await
        .unwrap();

    let env = store.load_env_values("srv-1").unwrap();
    let bundle = env.get(OAUTH_BUNDLE_KEY).expect("a bundle");
    assert!(bundle.contains("rt-1"), "the refresh token is kept");
    assert!(OAUTH_BUNDLE_KEY.starts_with("__"), "marked internal");
}

#[tokio::test]
async fn an_unknown_state_is_refused() {
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);

    let error = flow()
        .complete(&store, "never-issued", "the-code")
        .await
        .expect_err("unknown state");

    assert!(error.to_string().contains("unknown or expired"), "{error}");
}

#[tokio::test]
async fn a_state_is_consumed_even_when_the_exchange_fails() {
    // A code is single-use, so a retry with the same state could never work,
    // and keeping the entry would only hold a secret in memory.
    let (endpoint, state) = authority_with_challenge().await;
    state.refuse_exchange.store(true, Ordering::SeqCst);
    let store = store_with_remote(&endpoint);
    let flow = flow();
    let url = flow
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();
    let returned_state = authorize_param(&url, "state").unwrap();

    let _ = flow.complete(&store, &returned_state, "the-code").await;

    assert_eq!(flow.pending_count(), 0);
    let error = flow
        .complete(&store, &returned_state, "the-code")
        .await
        .expect_err("the state is gone");
    assert!(error.to_string().contains("unknown or expired"), "{error}");
}

#[tokio::test]
async fn a_refused_exchange_reports_what_the_token_endpoint_said() {
    // The failure body is where it says *why* — `invalid_grant` reads
    // differently from `invalid_client`, and a bare status tells them apart.
    let (endpoint, state) = authority_with_challenge().await;
    state.refuse_exchange.store(true, Ordering::SeqCst);
    let store = store_with_remote(&endpoint);
    let flow = flow();
    let url = flow
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();
    let returned_state = authorize_param(&url, "state").unwrap();

    let error = flow
        .complete(&store, &returned_state, "the-code")
        .await
        .expect_err("refused");

    assert!(error.to_string().contains("invalid_grant"), "{error}");
}

#[tokio::test]
async fn a_failed_exchange_stores_no_credential() {
    let (endpoint, state) = authority_with_challenge().await;
    state.refuse_exchange.store(true, Ordering::SeqCst);
    let store = store_with_remote(&endpoint);
    let flow = flow();
    let url = flow
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .unwrap();
    let returned_state = authorize_param(&url, "state").unwrap();

    let _ = flow.complete(&store, &returned_state, "the-code").await;

    assert!(store.load_env_values("srv-1").unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn detection_reports_oauth_when_discovery_reaches_an_authorize_endpoint() {
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);

    let detection = flow().detect(&store, "srv-1").await.expect("detect");

    assert_eq!(detection.kind, AuthKind::Oauth);
    assert!(
        detection
            .authorization_endpoint
            .is_some_and(|endpoint| endpoint.contains("/authorize"))
    );
}

#[tokio::test]
async fn detection_reports_the_grants_the_authorization_server_listed() {
    let (endpoint, _state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);

    let detection = flow().detect(&store, "srv-1").await.unwrap();

    assert!(
        detection
            .grant_types
            .iter()
            .any(|grant| grant == "authorization_code")
    );
}

// ---------------------------------------------------------------------------
// How a kind is spelled on the wire
// ---------------------------------------------------------------------------

#[test]
fn every_auth_kind_has_a_stable_spelling() {
    // A host branches on this string to decide which affordance to offer, so a
    // rename here is a silent behavior change at every one of them.
    assert_eq!(AuthKind::None.as_str(), "none");
    assert_eq!(AuthKind::Token.as_str(), "token");
    assert_eq!(AuthKind::Oauth.as_str(), "oauth");
}

#[test]
fn a_kind_serializes_as_the_same_spelling_it_reports() {
    // Otherwise the constant above and the wire form could drift apart, and
    // only one of them would be what a host actually receives.
    for kind in [AuthKind::None, AuthKind::Token, AuthKind::Oauth] {
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            json!(kind.as_str()),
            "{kind:?}"
        );
    }
}
