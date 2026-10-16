//! The default in-memory pending store, and the redacted shape a host
//! persists.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn parked(started_at: u64) -> PendingAuthorization {
    PendingAuthorization {
        server_id: "acme/linear".into(),
        code_verifier: "verifier-1".into(),
        client_id: "client-1".into(),
        client_secret: Some("secret-1".into()),
        token_endpoint: "https://auth.test/token".into(),
        redirect_uri: "https://host.test/oauth/callback".into(),
        started_at,
    }
}

fn now() -> u64 {
    super::super::tokens::now_unix()
}

#[tokio::test]
async fn a_parked_authorization_is_taken_once() {
    let at = now();
    let store = MemoryPendingStore::new();
    store.park("state-2", parked(at)).await.unwrap();

    assert_eq!(store.take("state-2").await.unwrap(), Some(parked(at)));
    assert_eq!(store.take("state-2").await.unwrap(), None);
}

#[tokio::test]
async fn peeking_leaves_the_authorization_parked() {
    let store = MemoryPendingStore::new();
    store.park("state-1", parked(now())).await.unwrap();

    let peeked = store.peek("state-1").await.unwrap().expect("parked");
    assert_eq!(peeked.server_id, "acme/linear");
    assert_eq!(store.count().await.unwrap(), 1);
    assert!(store.take("state-1").await.unwrap().is_some());
}

#[tokio::test]
async fn an_unknown_state_finds_nothing() {
    let store = MemoryPendingStore::new();

    assert_eq!(store.peek("never-issued").await.unwrap(), None);
    assert_eq!(store.take("never-issued").await.unwrap(), None);
    assert_eq!(store.count().await.unwrap(), 0);
}

#[tokio::test]
async fn an_expired_authorization_is_dropped() {
    let store = MemoryPendingStore::new();
    store
        .park("stale", parked(now() - PENDING_TTL_SECS - 1))
        .await
        .unwrap();

    assert_eq!(store.count().await.unwrap(), 0);
    assert_eq!(store.take("stale").await.unwrap(), None);
}

#[test]
fn expiry_is_measured_from_when_it_was_parked() {
    let at = 10_000;
    assert!(!parked(at).expired_at(at + PENDING_TTL_SECS));
    assert!(parked(at).expired_at(at + PENDING_TTL_SECS + 1));
    assert!(
        !parked(at).expired_at(0),
        "a clock behind the entry is not expiry"
    );
}

#[test]
fn debug_output_redacts_the_verifier_and_secret() {
    let rendered = format!("{:?}", parked(1));

    assert!(!rendered.contains("verifier-1"), "{rendered}");
    assert!(!rendered.contains("secret-1"), "{rendered}");
    assert!(rendered.contains("acme/linear"), "{rendered}");
}

#[test]
fn the_serde_representation_is_pinned() {
    let value = serde_json::to_value(parked(42)).unwrap();

    assert_eq!(
        value,
        serde_json::json!({
            "server_id": "acme/linear",
            "code_verifier": "verifier-1",
            "client_id": "client-1",
            "client_secret": "secret-1",
            "token_endpoint": "https://auth.test/token",
            "redirect_uri": "https://host.test/oauth/callback",
            "started_at": 42,
        })
    );
    let back: PendingAuthorization = serde_json::from_value(value).unwrap();
    assert_eq!(back, parked(42));
}

#[tokio::test]
async fn the_memory_stores_debug_output_never_shows_a_state() {
    let store = MemoryPendingStore::new();
    store.park("state-secret-1", parked(now())).await.unwrap();
    let flow = super::super::OAuthFlow::new(None)
        .unwrap()
        .with_pending_store(std::sync::Arc::new(store));

    let rendered = format!("{flow:?}");

    assert!(!rendered.contains("state-secret-1"), "{rendered}");
    assert!(!rendered.contains("verifier-1"), "{rendered}");
    assert!(rendered.contains("parked: 1"), "{rendered}");
}
