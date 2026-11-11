//! Choosing how a client authenticates at the token endpoint: a confidential
//! client with `client_secret_post`, or a public one with `none` when that is
//! all the authorization server accepts.

use super::*;
use crate::registry::OAuthBundle;

/// What the authorization server advertised and was sent.
#[derive(Debug, Default)]
struct Methods {
    advertised: Option<Value>,
    registrations: parking_lot::Mutex<Vec<Value>>,
    token_forms: parking_lot::Mutex<Vec<BTreeMap<String, String>>>,
}

impl Methods {
    fn last_registration(&self) -> Value {
        self.registrations
            .lock()
            .last()
            .cloned()
            .expect("a registration")
    }

    fn token_form(&self, index: usize) -> BTreeMap<String, String> {
        self.token_forms.lock()[index].clone()
    }
}

/// Accepts a registration the way the advertised list says it would: a
/// method a non-empty list leaves out is refused with
/// `invalid_client_metadata`, as `OnArrival` refuses `client_secret_post`.
async fn register(State(state): State<Arc<Methods>>, Json(body): Json<Value>) -> impl IntoResponse {
    state.registrations.lock().push(body.clone());
    let requested = body["token_endpoint_auth_method"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let accepted = state.advertised.as_ref().is_none_or(|advertised| {
        advertised.as_array().is_some_and(|methods| {
            methods.is_empty() || methods.iter().any(|method| method == &json!(requested))
        })
    });
    if !accepted {
        return (
            AxumStatus::BAD_REQUEST,
            Json(json!({
                "error": "invalid_client_metadata",
                "error_description": "Only public clients (token_endpoint_auth_method \"none\") are supported.",
            })),
        )
            .into_response();
    }
    Json(json!({
        "client_id": "client-1",
        "client_secret": "client-secret-1",
        "token_endpoint_auth_method": requested,
    }))
    .into_response()
}

/// Records each token request and mints a token that is already due for a
/// refresh, so one sign-in exercises both the exchange and the refresh.
async fn token(
    State(state): State<Arc<Methods>>,
    Form(form): Form<BTreeMap<String, String>>,
) -> impl IntoResponse {
    state.token_forms.lock().push(form);
    Json(json!({
        "access_token": "at-1",
        "refresh_token": "rt-1",
        "token_type": "Bearer",
        "expires_in": 0,
    }))
}

/// An MCP endpoint demanding OAuth from an authorization server on the same
/// origin that advertises `advertised` as its token endpoint auth methods, or
/// nothing when it is `None`.
async fn authority_advertising(advertised: Option<Value>) -> (String, Arc<Methods>) {
    let state = Arc::new(Methods {
        advertised,
        ..Methods::default()
    });
    let origin: Arc<parking_lot::Mutex<String>> = Arc::new(parking_lot::Mutex::new(String::new()));

    let app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get({
                let origin = Arc::clone(&origin);
                move || {
                    let origin = origin.lock().clone();
                    async move {
                        Json(json!({
                            "resource": format!("{origin}/mcp"),
                            "authorization_servers": [origin],
                        }))
                    }
                }
            }),
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
                        if let Some(advertised) = &state.advertised {
                            metadata["token_endpoint_auth_methods_supported"] = advertised.clone();
                        }
                        Json(metadata)
                    }
                }
            }),
        )
        .route("/register", post(register))
        .route("/token", post(token))
        .fallback({
            let origin = Arc::clone(&origin);
            move || {
                let origin = origin.lock().clone();
                async move {
                    let challenge = format!(
                        "Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource\""
                    );
                    (
                        AxumStatus::UNAUTHORIZED,
                        [("WWW-Authenticate", challenge)],
                        "unauthorized",
                    )
                        .into_response()
                }
            }
        })
        .with_state(Arc::clone(&state));

    let base = serve(app).await;
    *origin.lock() = base.clone();
    (format!("{base}/mcp"), state)
}

/// Signs in and refreshes once against `advertised`, returning what the
/// authorization server saw and the bundle left in the store.
async fn sign_in_and_refresh(advertised: Option<Value>) -> (Arc<Methods>, OAuthBundle) {
    let (endpoint, state) = authority_advertising(advertised).await;
    let store = store_with_remote(&endpoint);
    let flow = flow();

    let url = flow
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .expect("begin");
    let returned_state = authorize_param(&url, "state").unwrap();
    flow.complete(&store, &returned_state, "the-code")
        .await
        .expect("complete");
    assert!(flow.refresh(&store, "srv-1").await.expect("refresh"));

    let env = store.load_env_values("srv-1").unwrap();
    let bundle = serde_json::from_str(&env[OAUTH_BUNDLE_KEY]).unwrap();
    (state, bundle)
}

#[tokio::test]
async fn a_server_accepting_only_public_clients_gets_one_that_sends_no_secret() {
    let (state, bundle) = sign_in_and_refresh(Some(json!(["none"]))).await;

    assert_eq!(
        state.last_registration()["token_endpoint_auth_method"],
        json!("none")
    );
    assert_eq!(bundle.client_id, "client-1");
    assert_eq!(bundle.client_secret, None);

    let exchange = state.token_form(0);
    assert_eq!(
        exchange.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert_eq!(
        exchange.get("client_id").map(String::as_str),
        Some("client-1")
    );
    assert!(!exchange.contains_key("client_secret"), "{exchange:?}");

    let refresh = state.token_form(1);
    assert_eq!(
        refresh.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        refresh.get("client_id").map(String::as_str),
        Some("client-1")
    );
    assert!(!refresh.contains_key("client_secret"), "{refresh:?}");
}

#[tokio::test]
async fn a_server_accepting_both_gets_a_confidential_client() {
    let (state, bundle) = sign_in_and_refresh(Some(json!(["client_secret_post", "none"]))).await;

    assert_eq!(
        state.last_registration()["token_endpoint_auth_method"],
        json!("client_secret_post")
    );
    assert_eq!(bundle.client_secret.as_deref(), Some("client-secret-1"));
    for index in 0..2 {
        assert_eq!(
            state
                .token_form(index)
                .get("client_secret")
                .map(String::as_str),
            Some("client-secret-1")
        );
    }
}

#[tokio::test]
async fn a_server_that_lists_no_methods_gets_a_confidential_client() {
    let (state, bundle) = sign_in_and_refresh(None).await;

    assert_eq!(
        state.last_registration()["token_endpoint_auth_method"],
        json!("client_secret_post")
    );
    assert_eq!(bundle.client_secret.as_deref(), Some("client-secret-1"));
}

#[tokio::test]
async fn an_empty_method_list_is_read_as_listing_none() {
    let (state, _bundle) = sign_in_and_refresh(Some(json!([]))).await;

    assert_eq!(
        state.last_registration()["token_endpoint_auth_method"],
        json!("client_secret_post")
    );
}

#[tokio::test]
async fn a_server_accepting_neither_method_is_refused_before_registering() {
    let (endpoint, state) =
        authority_advertising(Some(json!(["client_secret_basic", "private_key_jwt"]))).await;
    let store = store_with_remote(&endpoint);

    let error = flow()
        .begin(&store, "srv-1", "http://127.0.0.1:7788/callback")
        .await
        .expect_err("no usable auth method");

    match error {
        Error::AuthDiscovery { detail, .. } => assert_eq!(
            detail,
            "no authorization server accepts client_secret_post or none at its token endpoint"
        ),
        other => panic!("expected an auth discovery error, got {other:?}"),
    }
    assert!(state.registrations.lock().is_empty());
}

#[tokio::test]
async fn a_server_accepting_neither_method_is_detected_as_wanting_a_static_token() {
    let (endpoint, _state) = authority_advertising(Some(json!(["client_secret_basic"]))).await;
    let store = store_with_remote(&endpoint);

    let detection = flow().detect(&store, "srv-1").await.unwrap();

    assert_eq!(detection.kind, AuthKind::Token);
}

#[tokio::test]
async fn a_server_accepting_only_public_clients_is_detected_as_wanting_sign_in() {
    let (endpoint, _state) = authority_advertising(Some(json!(["none"]))).await;
    let store = store_with_remote(&endpoint);

    let detection = flow().detect(&store, "srv-1").await.unwrap();

    assert_eq!(detection.kind, AuthKind::Oauth);
}

#[test]
fn an_incomplete_server_is_not_blamed_on_its_auth_methods() {
    let incomplete = tinymcp_bus::AuthorizationServerMetadata {
        issuer: "https://auth.test".into(),
        authorization_endpoint: None,
        token_endpoint: None,
        registration_endpoint: None,
        response_types_supported: Vec::new(),
        grant_types_supported: Vec::new(),
        code_challenge_methods_supported: Vec::new(),
        token_endpoint_auth_methods_supported: Some(vec!["client_secret_basic".into()]),
    };

    assert!(
        crate::registry::oauth::flow::no_drivable_server_reason(&[incomplete])
            .starts_with("no authorization server offers an authorize endpoint")
    );
    assert_eq!(
        crate::registry::oauth::flow::no_drivable_server_reason(&[]),
        "the server advertises no authorization server"
    );
}
