//! Unit tests for the bus adapter.
//!
//! The assertion that earns its place here is the one comparing the declared
//! manifest against [`tinymcp_bus::names::METHODS`]. A member served but not
//! declared is invisible to a host; one declared but not served is an
//! unknown-method failure the first time a user reaches for it. Neither shows
//! up in a type check, and neither shows up until something is already broken.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::config::ModuleConfig;
use super::service::McpService;
use tinymcp_bus::{McpClientConfig, McpServerConfig, names};

/// The members the manifest declares, in declaration order.
///
/// Read out of the manifest the macro generated rather than restated, so this
/// cannot drift from what a host is actually told.
fn declared_methods() -> Vec<String> {
    let slice = super::tinybus_module_manifest_v1();
    // The manifest is JSON in a byte slice the module owns for its lifetime.
    let bytes = unsafe { std::slice::from_raw_parts(slice.ptr, slice.len) };
    let manifest: serde_json::Value =
        serde_json::from_slice(bytes).expect("the manifest is valid json");

    manifest["methods"]
        .as_array()
        .expect("the manifest lists methods")
        .iter()
        .map(|method| {
            method
                .as_str()
                .expect("every method is a string")
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The manifest against the contract
// ---------------------------------------------------------------------------

#[test]
fn the_manifest_declares_exactly_the_members_the_contract_names() {
    // Order included: the contract lists them in dispatch order, and a member
    // that moved is worth noticing even though nothing depends on position.
    assert_eq!(declared_methods(), names::METHODS);
}

#[test]
fn the_manifest_claims_the_interface_the_contract_names() {
    let slice = super::tinybus_module_manifest_v1();
    let bytes = unsafe { std::slice::from_raw_parts(slice.ptr, slice.len) };
    let manifest: serde_json::Value = serde_json::from_slice(bytes).unwrap();

    assert_eq!(
        manifest["provides"],
        serde_json::json!([names::INTERFACE])
    );
}

#[test]
fn the_module_is_not_lazy() {
    // A host that loaded this wants its servers connected. Deferring the load
    // defers that until the first call — by which point an agent has already
    // been told it has no tools.
    let slice = super::tinybus_module_manifest_v1();
    let bytes = unsafe { std::slice::from_raw_parts(slice.ptr, slice.len) };
    let manifest: serde_json::Value = serde_json::from_slice(bytes).unwrap();

    assert_eq!(manifest["lazy"], serde_json::json!(false));
}

#[test]
fn the_module_serves_on_more_than_one_thread() {
    // A tool call on one server must not wait behind a slow call on another;
    // these are third-party endpoints and one being slow is routine.
    let slice = super::tinybus_module_manifest_v1();
    let bytes = unsafe { std::slice::from_raw_parts(slice.ptr, slice.len) };
    let manifest: serde_json::Value = serde_json::from_slice(bytes).unwrap();

    assert!(
        manifest["worker_threads"].as_u64().unwrap_or(0) > 1,
        "{manifest}"
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
    let service = McpService::new(ModuleConfig::default()).expect("the service builds");

    assert!(service.static_servers().is_empty());
}

#[test]
fn a_service_with_no_data_directory_persists_nothing() {
    // The right shape for a host that only wants its statically declared
    // servers: there is nothing to persist, and creating files for it would
    // leave state nobody asked for.
    let service = McpService::new(ModuleConfig::default()).expect("the service builds");

    assert!(service.dynamic().installed_list().unwrap().is_empty());
}

#[test]
fn a_service_with_a_data_directory_creates_its_stores() {
    let directory = tempfile::tempdir().unwrap();

    let _service = McpService::new(ModuleConfig {
        data_dir: Some(directory.path().to_path_buf()),
        client: McpClientConfig::default(),
    })
    .expect("the service builds");

    assert!(crate::Store::path_for(directory.path()).exists());
    assert!(crate::AuditStore::path_for(directory.path()).exists());
}

#[test]
fn a_service_registers_the_statically_declared_servers() {
    let service = McpService::new(ModuleConfig {
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

    let first = McpService::new(ModuleConfig {
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

    let second = McpService::new(ModuleConfig {
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
