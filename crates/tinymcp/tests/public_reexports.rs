//! Pins the crate-root paths a host imports the contract types from.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tinymcp::{
    CONTRACT_VERSION, MCP_CALL_RESULT_KIND, McpAuthChallenge, McpCallError, McpCallOutcome,
    RegistryFreshness,
};

#[test]
fn the_contract_types_resolve_at_the_crate_root_to_the_bus_definitions() {
    let challenge: tinymcp_bus::McpAuthChallenge = McpAuthChallenge {
        scheme: "Bearer".into(),
        realm: None,
        resource_metadata: None,
    };
    assert_eq!(challenge.scheme, "Bearer");

    let outcome: tinymcp_bus::McpCallOutcome = McpCallOutcome::failed(
        "docs",
        "search",
        McpCallError::new(tinymcp_bus::errors::TRANSPORT),
    );
    assert_eq!(outcome.kind, MCP_CALL_RESULT_KIND);
    assert_eq!(CONTRACT_VERSION, tinymcp_bus::CONTRACT_VERSION);

    let freshness: tinymcp_bus::RegistryFreshness = RegistryFreshness::LocalFallback;
    assert_ne!(freshness, RegistryFreshness::Live);
}

#[test]
fn the_pending_sign_in_store_api_resolves_at_the_crate_root() {
    use std::sync::Arc;
    use tinymcp::{
        MemoryPendingStore, OAuthFlow, PENDING_TTL_SECS, PendingAuthorization,
        PendingAuthorizationStore,
    };

    let store: Arc<dyn PendingAuthorizationStore> = Arc::new(MemoryPendingStore::new());
    let _flow = OAuthFlow::new(None)
        .expect("the flow builds")
        .with_pending_store(store);
    let parked = PendingAuthorization {
        server_id: "docs".into(),
        code_verifier: "v".into(),
        client_id: "c".into(),
        client_secret: None,
        token_endpoint: "https://auth.test/token".into(),
        redirect_uri: "https://host.test/cb".into(),
        started_at: 0,
    };
    assert!(parked.expired_at(PENDING_TTL_SECS + 1));
}
