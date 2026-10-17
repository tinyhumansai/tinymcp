//! The flow backed by a host's own secret store rather than the default
//! [`Store`]: what a multi-tenant host needs to keep PKCE, registration and
//! refresh state in its own per-tenant secrets.

use super::*;
use crate::registry::OAuthCredentialStore;
use std::io::{Read, Write};
use std::net::TcpListener;

/// A host's secret store: credentials in a map, keyed by whatever server id
/// the host chooses — here a tenant-qualified one.
#[derive(Debug, Default)]
struct HostSecrets {
    url: Option<String>,
    values: parking_lot::Mutex<BTreeMap<String, BTreeMap<String, String>>>,
    refuse_writes: std::sync::atomic::AtomicBool,
}

impl HostSecrets {
    fn remote(url: &str) -> Self {
        Self {
            url: Some(url.to_string()),
            ..Self::default()
        }
    }

    fn get(&self, server_id: &str, key: &str) -> Option<String> {
        self.values.lock().get(server_id)?.get(key).cloned()
    }
}

impl OAuthCredentialStore for HostSecrets {
    fn remote_url(
        &self,
        _server_id: &str,
    ) -> impl std::future::Future<Output = crate::Result<Option<String>>> + Send {
        std::future::poll_fn(move |_| std::task::Poll::Ready(Ok(self.url.clone())))
    }

    fn load_credentials(
        &self,
        server_id: &str,
    ) -> impl std::future::Future<Output = crate::Result<BTreeMap<String, String>>> + Send {
        std::future::poll_fn(move |_| {
            std::task::Poll::Ready(Ok(self
                .values
                .lock()
                .get(server_id)
                .cloned()
                .unwrap_or_default()))
        })
    }

    fn store_credentials(
        &self,
        server_id: &str,
        credentials: &BTreeMap<String, String>,
    ) -> impl std::future::Future<Output = crate::Result<()>> + Send {
        std::future::poll_fn(move |_| {
            if self.refuse_writes.load(Ordering::SeqCst) {
                return std::task::Poll::Ready(Err(Error::CredentialStore {
                    action: format!("writing credentials for {server_id}"),
                    detail: "the vault is sealed".into(),
                }));
            }
            self.values
                .lock()
                .insert(server_id.to_string(), credentials.clone());
            std::task::Poll::Ready(Ok(()))
        })
    }
}

const TENANT_SERVER: &str = "acme/linear";

#[tokio::test]
async fn a_host_store_backs_a_whole_sign_in() {
    let (endpoint, _state) = authority_with_challenge().await;
    let host = HostSecrets::remote(&endpoint);
    let flow = flow();

    let url = flow
        .begin(&host, TENANT_SERVER, "https://host.test/oauth/callback")
        .await
        .expect("begin");
    let state = authorize_param(&url, "state").unwrap();
    let server_id = flow
        .complete(&host, &state, "the-code")
        .await
        .expect("complete");

    assert_eq!(server_id, TENANT_SERVER);
    assert_eq!(
        host.get(TENANT_SERVER, "Authorization").as_deref(),
        Some("Bearer at-1")
    );
    let bundle: Value =
        serde_json::from_str(&host.get(TENANT_SERVER, OAUTH_BUNDLE_KEY).unwrap()).unwrap();
    assert_eq!(bundle["refresh_token"], json!("rt-1"));
    assert_eq!(bundle["client_id"], json!("client-1"));
}

/// The redirect carries no session, so a multi-tenant host has to learn which
/// tenant's store to complete into from the `state` alone — before consuming
/// it.
#[tokio::test]
async fn the_server_a_pending_state_belongs_to_can_be_read_before_completing() {
    let (endpoint, _state) = authority_with_challenge().await;
    let host = HostSecrets::remote(&endpoint);
    let flow = flow();
    let url = flow
        .begin(&host, TENANT_SERVER, "https://host.test/oauth/callback")
        .await
        .unwrap();
    let state = authorize_param(&url, "state").unwrap();

    assert_eq!(flow.pending_server(&state).as_deref(), Some(TENANT_SERVER));
    assert_eq!(flow.pending_count(), 1, "peeking does not consume");
    assert_eq!(flow.pending_server("never-issued"), None);
}

#[tokio::test]
async fn a_host_store_backs_refresh() {
    let requests = TokenRequests::default();
    let base = serve(token_endpoint(
        requests.clone(),
        json!({ "access_token": "fresh", "expires_in": 3600 }),
    ))
    .await;
    let host = HostSecrets::default();
    let bundle = json!({
        "refresh_token": "r1",
        "client_id": "cli-1",
        "client_secret": null,
        "token_endpoint": format!("{base}/token"),
        "expires_at": 1,
    });
    host.values.lock().insert(
        TENANT_SERVER.to_string(),
        BTreeMap::from([(OAUTH_BUNDLE_KEY.to_string(), bundle.to_string())]),
    );

    let refreshed = refresh_if_expired(&host, &reqwest::Client::new(), TENANT_SERVER)
        .await
        .expect("refresh");

    assert!(refreshed);
    assert_eq!(
        host.get(TENANT_SERVER, "Authorization").as_deref(),
        Some("Bearer fresh")
    );
    assert_eq!(requests.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_host_store_failure_is_reported_as_a_store_error() {
    let (endpoint, _state) = authority_with_challenge().await;
    let host = HostSecrets::remote(&endpoint);
    host.refuse_writes.store(true, Ordering::SeqCst);
    let flow = flow();
    let url = flow
        .begin(&host, TENANT_SERVER, "https://host.test/oauth/callback")
        .await
        .unwrap();
    let state = authorize_param(&url, "state").unwrap();

    let error = flow
        .complete(&host, &state, "the-code")
        .await
        .expect_err("a sealed vault");

    assert!(matches!(error, Error::CredentialStore { .. }), "{error:?}");
    assert_eq!(error.wire_name(), tinymcp_bus::errors::CREDENTIAL_STORE);
    assert!(error.to_string().contains("sealed"), "{error}");
}

#[tokio::test]
async fn a_host_store_with_no_remote_endpoint_cannot_be_signed_in_to() {
    let error = flow()
        .begin(
            &HostSecrets::default(),
            TENANT_SERVER,
            "https://host.test/cb",
        )
        .await
        .expect_err("no endpoint");
    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Detection agrees with what `begin` can drive
// ---------------------------------------------------------------------------

/// Slack's MCP endpoint challenges properly and advertises an authorize
/// endpoint, but no registration endpoint — so there is no client to mint and
/// `begin` refuses. Offering a Sign in button for it is offering a dead end;
/// detection has to answer "paste a token" instead.
#[tokio::test]
async fn detection_offers_a_token_when_there_is_no_dynamic_registration() {
    let (endpoint, state) = authority_with_challenge().await;
    state.without_registration.store(true, Ordering::SeqCst);
    let store = store_with_remote(&endpoint);
    let flow = flow();

    let detection = flow.detect(&store, "srv-1").await.expect("detect");

    assert_eq!(detection.kind, AuthKind::Token, "{detection:?}");
    assert!(
        flow.begin(&store, "srv-1", "http://127.0.0.1:1/cb")
            .await
            .is_err(),
        "begin refuses the same server, so detection must not offer it"
    );
}

// ---------------------------------------------------------------------------
// A failure body never carries what was submitted
// ---------------------------------------------------------------------------

/// A refresh whose token endpoint fails with `reply`, echoing the form back.
async fn rejected_refresh(reply: String) -> Error {
    let app = Router::new().route(
        "/token",
        post(move |body: String| {
            let reply = reply.replace("{form}", &body);
            async move { (AxumStatus::BAD_REQUEST, reply) }
        }),
    );
    let base = serve(app).await;
    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1-secret-refresh"));
    refresh_if_expired(&store, &reqwest::Client::new(), "srv-1")
        .await
        .expect_err("a rejected refresh")
}

/// Some token endpoints echo the submitted form in their error body — the
/// refresh token, the client secret. That body reaches logs and user
/// interfaces through the error, so only the standard OAuth `error` and
/// `error_description` survive: they are the *why*, and carry no secret.
#[tokio::test]
async fn a_token_endpoint_failure_keeps_the_reason_but_not_an_echo() {
    let error = rejected_refresh(
        json!({
            "error": "invalid_grant",
            "error_description": "refresh token revoked",
            "request": "{form}",
        })
        .to_string(),
    )
    .await;

    let Error::Http { body, .. } = &error else {
        panic!("expected an http error, got {error:?}");
    };
    assert!(body.contains("invalid_grant"), "{body}");
    assert!(body.contains("refresh token revoked"), "{body}");
    assert!(!body.contains("r1-secret-refresh"), "{body}");
    assert!(!body.contains("sec-1"), "{body}");
}

#[tokio::test]
async fn a_non_json_token_endpoint_failure_is_not_echoed() {
    let error = rejected_refresh("you sent: {form}".to_string()).await;
    let rendered = error.to_string();
    assert!(!rendered.contains("r1-secret-refresh"), "{rendered}");
    assert!(!rendered.contains("sec-1"), "{rendered}");
}

// ---------------------------------------------------------------------------
// Refusing internal endpoints a hostile server advertises
// ---------------------------------------------------------------------------

/// The authorization server's endpoints come from the server being signed in
/// to, and the flow POSTs to them from the host. A host serving untrusted
/// servers opts into refusing internal targets; the loopback authority here is
/// exactly such a target, so nothing may be registered with it.
#[tokio::test]
async fn a_guarded_flow_refuses_an_internal_authorization_server() {
    // The server URL itself is now refused first when it is internal (see
    // `a_guarded_flow_refuses_an_internal_server_url_before_probing_it`), so
    // the endpoints a public server could advertise are checked directly:
    // each internal one is refused by name, before anything is registered.
    let flow = flow().require_public_endpoints();
    let public = "https://8.8.8.8";
    for (authorize, token, register, what) in [
        (
            "http://127.0.0.1/authorize".to_string(),
            format!("{public}/token"),
            format!("{public}/register"),
            "authorization",
        ),
        (
            format!("{public}/authorize"),
            "https://169.254.169.254/token".to_string(),
            format!("{public}/register"),
            "token",
        ),
        (
            format!("{public}/authorize"),
            format!("{public}/token"),
            "https://10.0.0.1/register".to_string(),
            "registration",
        ),
    ] {
        let error = flow
            .registration_client(&authorize, &token, &register)
            .await
            .expect_err(what);
        assert!(
            error
                .to_string()
                .contains(&format!("{what} endpoint refused")),
            "{error}"
        );
    }
    assert!(
        flow.registration_client(
            &format!("{public}/authorize"),
            &format!("{public}/token"),
            &format!("{public}/register"),
        )
        .await
        .expect("public endpoints pass")
        .is_some()
    );
}

#[tokio::test]
async fn a_guarded_flow_refresh_client_does_not_follow_redirects() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let endpoint = format!("http://{}/refresh", listener.local_addr().expect("address"));
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept request");
        let mut request = [0; 1024];
        let _ = socket.read(&mut request).expect("read request");
        socket
            .write_all(
                b"HTTP/1.1 302 Found\r\nLocation: /redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .expect("write redirect");
    });

    let response = flow()
        .require_public_endpoints()
        .http()
        .get(endpoint)
        .send()
        .await
        .expect("receive redirect response");

    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    server.join().expect("server thread");
}

#[tokio::test]
async fn public_endpoint_clients_validate_and_pin_registration_and_token_hosts() {
    let flow = flow().require_public_endpoints();
    let registration = flow
        .registration_client(
            "https://8.8.8.8/authorize",
            "https://1.1.1.1/token",
            "https://9.9.9.9/register",
        )
        .await
        .expect("validate public endpoints");
    assert!(registration.is_some());
    assert!(
        flow.token_client("https://8.8.8.8/token")
            .await
            .expect("build token client")
            .is_some()
    );
}

#[tokio::test]
async fn desktop_flow_keeps_the_default_clients() {
    let flow = flow();
    assert!(
        flow.registration_client("not a url", "not a url", "not a url")
            .await
            .expect("desktop flow skips public endpoint checks")
            .is_none()
    );
    assert!(
        flow.token_client("not a url")
            .await
            .expect("desktop flow skips public endpoint checks")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Refusing an internal server URL itself
// ---------------------------------------------------------------------------

/// The flow probes the server URL from the host before anything else, so a
/// guarded flow checks that URL like the endpoints the server advertises:
/// a user-supplied server address must not reach an internal service.
#[tokio::test]
async fn a_guarded_flow_refuses_an_internal_server_url_before_probing_it() {
    let (endpoint, state) = authority_with_challenge().await;
    let store = store_with_remote(&endpoint);
    let flow = flow().require_public_endpoints();

    let begun = flow
        .begin(&store, "srv-1", "https://host.test/cb")
        .await
        .expect_err("a loopback server url");
    let detected = flow
        .detect(&store, "srv-1")
        .await
        .expect_err("a loopback server url");

    for error in [begun, detected] {
        assert!(
            error.to_string().contains("MCP endpoint refused"),
            "{error}"
        );
    }
    assert_eq!(state.registrations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_public_endpoint_guard_is_usable_by_a_host() {
    let plain = crate::registry::guard_public_endpoint("http://example.com/mcp", "MCP")
        .await
        .expect_err("plain http");
    let internal = crate::registry::guard_public_endpoint("https://169.254.169.254/mcp", "MCP")
        .await
        .expect_err("the metadata address");
    let pinned = crate::registry::guard_public_endpoint("https://93.184.215.14/mcp", "MCP")
        .await
        .expect("a public literal address");

    assert!(plain.to_string().contains("must use https"), "{plain}");
    assert!(
        internal.to_string().contains("disallowed address"),
        "{internal}"
    );
    assert_eq!(pinned.len(), 1);
}
