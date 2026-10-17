//! Pins the crate-root paths a host imports the contract types from.

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

#[tokio::test]
async fn the_public_endpoint_guard_resolves_at_the_crate_root() {
    let refused = tinymcp::guard_public_endpoint("http://example.com/mcp", "MCP").await;
    assert!(refused.is_err());
}
