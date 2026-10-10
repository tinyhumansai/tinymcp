//! [`OAuthFlow::refresh`]: the flow's own refresh, which re-applies the
//! public-endpoint guard, and the public [`OAuthBundle`] wire form.

use super::*;
use crate::registry::OAuthBundle;

#[test]
fn the_bundle_wire_form_is_pinned_and_round_trips() {
    let bundle = OAuthBundle {
        refresh_token: Some("r1".into()),
        client_id: "cli-1".into(),
        client_secret: None,
        token_endpoint: "https://auth.example/token".into(),
        expires_at: 42,
    };
    let wire = serde_json::to_value(&bundle).unwrap();
    assert_eq!(
        wire,
        json!({
            "refresh_token": "r1",
            "client_id": "cli-1",
            "client_secret": null,
            "token_endpoint": "https://auth.example/token",
            "expires_at": 42,
        })
    );
    assert_eq!(serde_json::from_value::<OAuthBundle>(wire).unwrap(), bundle);
}

#[test]
fn the_bundle_debug_output_hides_its_secrets() {
    let bundle = OAuthBundle {
        refresh_token: Some("refresh-secret".into()),
        client_id: "cli-1".into(),
        client_secret: Some("client-secret".into()),
        token_endpoint: "https://auth.example/token".into(),
        expires_at: 42,
    };
    let shown = format!("{bundle:?} {bundle:#?}");
    assert!(!shown.contains("refresh-secret"), "{shown}");
    assert!(!shown.contains("client-secret"), "{shown}");
    assert!(
        shown.contains("cli-1") && shown.contains("[redacted]"),
        "{shown}"
    );

    let bare = OAuthBundle {
        refresh_token: None,
        client_secret: None,
        ..bundle
    };
    assert!(format!("{bare:?}").contains("refresh_token: None"));
}

#[test]
fn a_stored_bundle_reads_back_as_the_public_type() {
    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, "https://auth.example/token", Some("r1"));
    let env = store.load_env_values("srv-1").unwrap();
    let bundle: OAuthBundle = serde_json::from_str(&env[OAUTH_BUNDLE_KEY]).unwrap();
    assert_eq!(bundle.client_secret.as_deref(), Some("sec-1"));
    assert_eq!(bundle.refresh_token.as_deref(), Some("r1"));
}

#[tokio::test]
async fn the_flow_refreshes_an_expired_token_when_unguarded() {
    let requests = TokenRequests::default();
    let base = serve(token_endpoint(
        requests.clone(),
        json!({ "access_token": "fresh", "expires_in": 3600 }),
    ))
    .await;
    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1"));

    let flow = OAuthFlow::new(None).unwrap();
    assert!(flow.refresh(&store, "srv-1").await.unwrap());
    assert_eq!(requests.count.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.load_env_values("srv-1").unwrap()["Authorization"],
        "Bearer fresh"
    );

    assert!(!flow.refresh(&store, "srv-1").await.unwrap());
    assert_eq!(requests.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_guarded_flow_refuses_a_loopback_token_endpoint_and_sends_nothing() {
    let requests = TokenRequests::default();
    let base = serve(token_endpoint(
        requests.clone(),
        json!({ "access_token": "fresh" }),
    ))
    .await;
    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1"));

    let flow = OAuthFlow::new(None).unwrap().require_public_endpoints();
    let error = flow.refresh(&store, "srv-1").await.unwrap_err();
    assert!(
        matches!(&error, Error::MalformedResponse { detail } if detail.starts_with("token endpoint refused")),
        "{error}"
    );
    assert_eq!(requests.count.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.load_env_values("srv-1").unwrap()["Authorization"],
        "Bearer stale"
    );
}

#[tokio::test]
async fn the_guarded_flow_refuses_an_https_endpoint_on_an_internal_address() {
    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, "https://127.0.0.1:1/token", Some("r1"));

    let flow = OAuthFlow::new(None).unwrap().require_public_endpoints();
    let error = flow.refresh(&store, "srv-1").await.unwrap_err();
    assert!(error.to_string().contains("disallowed address"), "{error}");
}

#[tokio::test]
async fn the_guarded_flow_leaves_a_token_that_needs_no_refresh_alone() {
    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, "http://127.0.0.1:1/token", None);

    let flow = OAuthFlow::new(None).unwrap().require_public_endpoints();
    assert!(!flow.refresh(&store, "srv-1").await.unwrap());
    assert!(
        !flow
            .refresh(&store_with_remote("https://x.test/mcp"), "srv-1")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn an_unreadable_bundle_is_reported_by_the_flow() {
    let store = store_with_remote("https://example.test/mcp");
    store
        .set_env_values(
            "srv-1",
            &BTreeMap::from([(OAUTH_BUNDLE_KEY.to_string(), "not json".to_string())]),
        )
        .unwrap();
    let error = OAuthFlow::new(None)
        .unwrap()
        .refresh(&store, "srv-1")
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("stored oauth bundle is unreadable")
    );
}

/// A store whose bundle is replaced by a newer sign-in after the first read.
struct SupersededStore {
    inner: Store,
    reads: AtomicUsize,
    writes: AtomicUsize,
}

impl crate::registry::OAuthCredentialStore for SupersededStore {
    async fn remote_url(&self, server_id: &str) -> crate::Result<Option<String>> {
        self.inner.remote_url(server_id).await
    }

    async fn load_credentials(&self, server_id: &str) -> crate::Result<BTreeMap<String, String>> {
        let mut env = self.inner.load_credentials(server_id).await?;
        if self.reads.fetch_add(1, Ordering::SeqCst) >= 1 {
            let newer = json!({
                "refresh_token": "r-newer",
                "client_id": "cli-2",
                "client_secret": null,
                "token_endpoint": "https://auth.example/token",
                "expires_at": u64::MAX / 2,
            });
            env.insert(OAUTH_BUNDLE_KEY.to_string(), newer.to_string());
            env.insert("Authorization".to_string(), "Bearer newer".to_string());
        }
        Ok(env)
    }

    async fn store_credentials(
        &self,
        server_id: &str,
        credentials: &BTreeMap<String, String>,
    ) -> crate::Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.store_credentials(server_id, credentials).await
    }
}

#[tokio::test]
async fn a_refresh_does_not_overwrite_a_sign_in_stored_while_it_waited() {
    let requests = TokenRequests::default();
    let base = serve(token_endpoint(
        requests.clone(),
        json!({ "access_token": "fresh", "expires_in": 3600 }),
    ))
    .await;
    let inner = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&inner, &format!("{base}/token"), Some("r1"));
    let store = SupersededStore {
        inner,
        reads: AtomicUsize::new(0),
        writes: AtomicUsize::new(0),
    };

    let flow = OAuthFlow::new(None).unwrap();
    assert!(!flow.refresh(&store, "srv-1").await.unwrap());
    assert_eq!(requests.count.load(Ordering::SeqCst), 1);
    assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.inner.load_env_values("srv-1").unwrap()["Authorization"],
        "Bearer stale"
    );
}

#[tokio::test]
async fn exchanging_a_bundle_without_a_refresh_token_sends_nothing() {
    let requests = TokenRequests::default();
    let base = serve(token_endpoint(
        requests.clone(),
        json!({ "access_token": "fresh", "expires_in": 3600 }),
    ))
    .await;
    let store = store_with_remote("https://example.test/mcp");
    store_expired_bundle(&store, &format!("{base}/token"), Some("r1"));
    let http = reqwest::Client::new();
    for refresh_token in [None, Some("  ".to_string())] {
        let bundle = OAuthBundle {
            refresh_token,
            client_id: "cli-1".into(),
            client_secret: None,
            token_endpoint: format!("{base}/token"),
            expires_at: 1,
        };
        assert!(
            !crate::registry::oauth::tokens::exchange_refresh(&store, &http, "srv-1", &bundle)
                .await
                .unwrap()
        );
    }
    assert_eq!(requests.count.load(Ordering::SeqCst), 0);
}
