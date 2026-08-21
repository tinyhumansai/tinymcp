//! Unit tests for the bus adapter.
//!
//! The assertion that earns its place here compares the members the service
//! actually serves against [`tinymcp_bus::names::METHODS`]. A member served but
//! not named in the contract is invisible to a host; one named but not served
//! is an unknown-method failure the first time a user reaches for it. Neither
//! shows up in a type check, and neither shows up until something is broken.
//!
//! The served list comes from the interface the macro generated, not from a
//! list restated here — a restatement would agree with itself no matter what
//! the code did.
//!
//! The *manifest* carries the same names a third time, because the macro needs
//! literals. Nothing safe can read those bytes back, so they are verified
//! end-to-end by `examples/verify_module.rs`, which loads the built library
//! through `TinyBus` and calls it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tinybus::service::Interface;

use super::config::ModuleConfig;
use super::service::McpService;
use tinymcp_bus::{McpClientConfig, McpServerConfig, names};

/// A service over nothing, for inspecting its interface.
fn service() -> McpService {
    McpService::new(&ModuleConfig::default()).expect("the service builds")
}

// ---------------------------------------------------------------------------
// The interface against the contract
// ---------------------------------------------------------------------------

#[test]
fn the_service_serves_exactly_the_members_the_contract_names() {
    // Order included: the contract lists them in dispatch order, and a member
    // that moved is worth noticing even though nothing depends on position.
    let served: Vec<String> = service()
        .members()
        .into_iter()
        .map(|member| member.as_str().to_string())
        .collect();

    assert_eq!(served, names::METHODS);
}

#[test]
fn the_service_claims_the_interface_the_contract_names() {
    assert_eq!(service().name().as_str(), names::INTERFACE);
}

#[test]
fn every_served_member_is_reachable_by_its_contract_constant() {
    // Spelled through the constants rather than as strings, so a rename in the
    // contract fails to compile here rather than failing at a host.
    let served = service().members();
    let has = |member: &str| served.iter().any(|found| found.as_str() == member);

    for member in [
        names::methods::REGISTRY_SEARCH,
        names::methods::INSTALL,
        names::methods::CONNECT,
        names::methods::TOOL_CALL,
        names::methods::OAUTH_BEGIN,
        names::methods::SETUP_INSTALL_AND_CONNECT,
        names::methods::STATIC_CALL_TOOL,
        names::methods::AUDIT_LIST_WRITES,
    ] {
        assert!(has(member), "{member} is not served");
    }
}

#[test]
fn the_authorization_member_keeps_its_capitalisation() {
    // Derived from the method name it would be `OauthBegin`, which is not what
    // the contract says. It carries an explicit name for that reason, and this
    // is what notices if that annotation is dropped.
    assert!(
        service()
            .members()
            .iter()
            .any(|member| member.as_str() == "OAuthBegin")
    );
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[test]
fn an_absent_configuration_decodes_to_a_working_default() {
    // The loader passes the empty object when a host supplies nothing.
    let config: ModuleConfig = serde_json::from_str("{}").expect("the empty object decodes");

    assert_eq!(config.data_dir, None);
    assert!(config.client.servers.is_empty());
    assert!(config.client.enabled);
}

#[test]
fn a_configuration_round_trips() {
    let config = ModuleConfig {
        data_dir: Some("/data/tinymcp".into()),
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "weather".into(),
                endpoint: "https://example.test/mcp".into(),
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    };

    let encoded = serde_json::to_value(&config).unwrap();
    let decoded: ModuleConfig = serde_json::from_value(encoded).unwrap();

    assert_eq!(decoded.data_dir, config.data_dir);
    assert_eq!(decoded.client.servers.len(), 1);
}

// ---------------------------------------------------------------------------
// Building the service
// ---------------------------------------------------------------------------

#[test]
fn a_service_builds_from_nothing() {
    assert!(service().static_servers().is_empty());
}

#[test]
fn a_service_with_no_data_directory_persists_nothing() {
    // The right shape for a host that only wants its statically declared
    // servers: there is nothing to persist, and creating files for it would
    // leave state nobody asked for.
    assert!(service().dynamic().installed_list().unwrap().is_empty());
}

#[test]
fn a_service_with_a_data_directory_creates_its_stores() {
    let directory = tempfile::tempdir().unwrap();

    let _service = McpService::new(&ModuleConfig {
        data_dir: Some(directory.path().to_path_buf()),
        client: McpClientConfig::default(),
    })
    .expect("the service builds");

    assert!(crate::Store::path_for(directory.path()).exists());
    assert!(crate::AuditStore::path_for(directory.path()).exists());
}

#[test]
fn a_service_registers_the_statically_declared_servers() {
    let service = McpService::new(&ModuleConfig {
        data_dir: None,
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "weather".into(),
                endpoint: "https://example.test/mcp".into(),
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    })
    .expect("the service builds");

    assert_eq!(service.static_servers().len(), 1);
    assert!(service.static_servers().get("weather").is_some());
}

#[test]
fn a_service_reopens_an_existing_store() {
    let directory = tempfile::tempdir().unwrap();

    let first = McpService::new(&ModuleConfig {
        data_dir: Some(directory.path().to_path_buf()),
        client: McpClientConfig::default(),
    })
    .unwrap();
    first
        .audit()
        .record(&tinymcp_bus::NewMcpWriteRecord {
            timestamp_ms: 1_000,
            client_info: "claude".into(),
            tool_name: "memory_write".into(),
            args_summary: serde_json::json!(null),
            resulting_chunk_id: None,
            success: true,
            error_message: None,
        })
        .unwrap();
    drop(first);

    let second = McpService::new(&ModuleConfig {
        data_dir: Some(directory.path().to_path_buf()),
        client: McpClientConfig::default(),
    })
    .unwrap();

    assert_eq!(
        second
            .audit()
            .list(&tinymcp_bus::McpWriteListQuery::default())
            .unwrap()
            .len(),
        1
    );
}
