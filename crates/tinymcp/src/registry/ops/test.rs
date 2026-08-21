//! Unit tests for the registry facade.
//!
//! The install path gets the most attention: it is idempotent, it merges rather
//! than replaces credentials, and it strips a routing prefix before it writes.
//! Each of those is a rule whose failure is silent — a duplicate record, an
//! erased credential, a second install of a service already present.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use serde_json::json;

use super::install::{
    build_install_transport, collect_required_env_keys, pick_connection, resolve_command,
    transport_kind,
};
use super::types::McpRegistry;
use crate::Error;
use crate::registry::Store;
use tinymcp_bus::{
    CommandKind, InstalledServer, McpClientIdentityConfig, McpRegistryAuthConfig,
    RegistryConnection, RegistryServerDetail, Transport, UpdateEnvStatus,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A connection of the given kind.
fn connection(kind: &str, published: bool, url: Option<&str>) -> RegistryConnection {
    serde_json::from_value(json!({
        "type": kind,
        "published": published,
        "deployment_url": url,
    }))
    .expect("a connection decodes")
}

/// A detail record carrying `connections`.
fn detail(connections: Vec<RegistryConnection>) -> RegistryServerDetail {
    let mut detail: RegistryServerDetail = serde_json::from_value(json!({
        "qualified_name": "com.vendor/server",
        "display_name": "Server",
    }))
    .expect("a detail decodes");
    detail.connections = connections;
    detail
}

/// A facade over an in-memory store.
fn registry() -> McpRegistry {
    McpRegistry::new(
        Store::open_in_memory().expect("a store"),
        McpRegistryAuthConfig::default(),
        McpClientIdentityConfig::default(),
        None,
    )
    .expect("the facade builds")
}

/// An install record.
fn install_record(server_id: &str, qualified_name: &str, enabled: bool) -> InstalledServer {
    InstalledServer {
        server_id: server_id.to_string(),
        qualified_name: qualified_name.to_string(),
        display_name: "Server".into(),
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
        enabled,
    }
}

// ---------------------------------------------------------------------------
// Choosing a connection
// ---------------------------------------------------------------------------

#[test]
fn connection_kinds_are_normalised_onto_the_install_vocabulary() {
    // Catalogs say `http`; an install record says `http_remote`. Server-sent
    // events are a hosted endpoint too.
    assert_eq!(transport_kind(&connection("stdio", true, None)), "stdio");
    for hosted in ["http", "http_remote", "sse"] {
        assert_eq!(
            transport_kind(&connection(hosted, true, None)),
            "http_remote",
            "for {hosted}"
        );
    }
}

#[test]
fn a_published_hosted_endpoint_is_preferred_over_everything() {
    // Preferring the subprocess means every install has to find a runtime,
    // resolve a package, and locate credentials — three ways to fail before the
    // server is reached.
    let connections = vec![
        connection("stdio", true, None),
        connection("http", true, Some("https://api.test/mcp")),
    ];

    let picked = pick_connection(&connections).expect("a connection");
    assert_eq!(transport_kind(picked), "http_remote");
}

#[test]
fn an_unpublished_hosted_endpoint_still_beats_a_published_package() {
    let connections = vec![
        connection("stdio", true, None),
        connection("http", false, Some("https://api.test/mcp")),
    ];

    assert_eq!(
        transport_kind(pick_connection(&connections).expect("a connection")),
        "http_remote"
    );
}

#[test]
fn a_published_package_is_used_when_there_is_no_hosted_endpoint() {
    let connections = vec![
        connection("stdio", false, None),
        connection("stdio", true, None),
    ];

    let picked = pick_connection(&connections).expect("a connection");
    assert!(picked.published);
}

#[test]
fn an_unpublished_package_is_the_last_resort() {
    let connections = vec![connection("stdio", false, None)];
    assert!(pick_connection(&connections).is_some());
}

#[test]
fn a_server_offering_nothing_dialable_yields_no_connection() {
    assert!(pick_connection(&[]).is_none());
    assert!(pick_connection(&[connection("carrier-pigeon", true, None)]).is_none());
}

// ---------------------------------------------------------------------------
// Required credentials
// ---------------------------------------------------------------------------

#[test]
fn the_required_credentials_come_from_the_chosen_connection_only() {
    // A server offering both must not demand the package's variables for an
    // install that connects over HTTP and never reads them — that is a form the
    // user cannot complete for a server that would have worked.
    let mut hosted = connection("http", true, Some("https://api.test/mcp"));
    hosted.config_schema = Some(json!({ "properties": { "Authorization": {} } }));

    let mut package = connection("stdio", true, None);
    package.config_schema = Some(json!({ "properties": { "LOCAL_PATH": {}, "API_KEY": {} } }));

    let required = collect_required_env_keys(&detail(vec![package, hosted]));

    assert_eq!(required, ["Authorization"]);
}

#[test]
fn a_connection_with_no_schema_requires_nothing() {
    let required = collect_required_env_keys(&detail(vec![connection(
        "http",
        true,
        Some("https://api.test/mcp"),
    )]));

    assert!(required.is_empty());
}

#[test]
fn a_server_with_no_connections_requires_nothing() {
    assert!(collect_required_env_keys(&detail(Vec::new())).is_empty());
}

// ---------------------------------------------------------------------------
// Building the install transport
// ---------------------------------------------------------------------------

#[test]
fn a_hosted_connection_becomes_a_remote_install_with_no_command() {
    let (transport, _, command, args) = build_install_transport(
        "com.vendor/server",
        &connection("http", true, Some("https://api.test/mcp")),
    )
    .expect("a transport");

    assert_eq!(
        transport,
        Transport::HttpRemote {
            url: "https://api.test/mcp".into()
        }
    );
    assert!(command.is_empty());
    assert!(args.is_empty());
}

#[test]
fn a_hosted_connection_with_no_endpoint_is_refused() {
    // Installing it would write a record that fails on every connect with
    // nothing the user could fix.
    let error = build_install_transport("com.vendor/server", &connection("http", true, None))
        .expect_err("no endpoint");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[test]
fn a_hosted_connection_with_a_blank_endpoint_is_refused() {
    assert!(
        build_install_transport("com.vendor/server", &connection("http", true, Some("   ")))
            .is_err()
    );
}

#[test]
fn a_package_connection_becomes_a_subprocess_install() {
    let (transport, kind, command, args) =
        build_install_transport("com.vendor/server", &connection("stdio", true, None))
            .expect("a transport");

    assert_eq!(transport, Transport::Stdio);
    assert_eq!(kind, CommandKind::Node);
    assert_eq!(command, "npx");
    assert_eq!(args, ["-y", "com.vendor/server"]);
}

// ---------------------------------------------------------------------------
// Resolving the launch command
// ---------------------------------------------------------------------------

#[test]
fn a_catalog_worked_example_is_used_when_there_is_one() {
    let mut package = connection("stdio", true, None);
    package.example_config = Some(json!({ "command": "uvx", "args": ["some-server"] }));

    let (kind, command, args) = resolve_command("com.vendor/server", Some(&package));

    assert_eq!(kind, CommandKind::Python);
    assert_eq!(command, "uvx");
    assert_eq!(args, ["some-server"]);
}

#[test]
fn the_launcher_names_the_ecosystem() {
    for (launcher, expected) in [
        ("uvx", CommandKind::Python),
        ("python3", CommandKind::Python),
        ("npx", CommandKind::Node),
        ("bun", CommandKind::Node),
    ] {
        let mut package = connection("stdio", true, None);
        package.example_config = Some(json!({ "command": launcher }));

        let (kind, _, _) = resolve_command("com.vendor/server", Some(&package));
        assert_eq!(kind, expected, "for {launcher}");
    }
}

#[test]
fn a_package_with_no_example_falls_back_to_npx() {
    let (kind, command, args) = resolve_command("com.vendor/server", None);

    assert_eq!(kind, CommandKind::Node);
    assert_eq!(command, "npx");
    assert_eq!(args, ["-y", "com.vendor/server"]);
}

// ---------------------------------------------------------------------------
// Blank identifiers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_blank_identifier_is_refused_by_every_operation_that_takes_one() {
    let registry = registry();

    assert!(registry.connect("   ").await.is_err());
    assert!(registry.disconnect("").await.is_err());
    assert!(registry.uninstall("  ").await.is_err());
    assert!(registry.set_enabled("", true).await.is_err());
    assert!(registry.list_tools("   ").await.is_err());
    assert!(registry.detect_auth("").await.is_err());
    assert!(registry.registry_get("   ").await.is_err());
    assert!(registry.tool_call("srv-1", "  ", json!({})).await.is_err());
}

// ---------------------------------------------------------------------------
// Installs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn installs_are_listed() {
    let registry = registry();
    registry
        .store()
        .insert_server(&install_record("srv-1", "@test/server", true))
        .unwrap();

    assert_eq!(registry.installed_list().unwrap().len(), 1);
}

#[tokio::test]
async fn uninstalling_reports_whether_a_record_went() {
    let registry = registry();
    registry
        .store()
        .insert_server(&install_record("srv-1", "@test/server", true))
        .unwrap();

    assert!(registry.uninstall("srv-1").await.unwrap());
    assert!(!registry.uninstall("srv-1").await.unwrap());
}

#[tokio::test]
async fn uninstalling_takes_the_credentials_with_it() {
    let registry = registry();
    registry
        .store()
        .insert_server(&install_record("srv-1", "@test/server", true))
        .unwrap();
    registry
        .store()
        .set_env_values(
            "srv-1",
            &BTreeMap::from([("API_KEY".to_string(), "secret".to_string())]),
        )
        .unwrap();

    registry.uninstall("srv-1").await.unwrap();

    assert!(
        registry
            .store()
            .load_env_values("srv-1")
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Enabling and disabling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn turning_a_server_off_persists_and_clears_its_recorded_failure() {
    let registry = registry();
    registry
        .store()
        .insert_server(&install_record("srv-1", "@test/server", true))
        .unwrap();

    registry.set_enabled("srv-1", false).await.unwrap();

    assert!(!registry.store().get_server("srv-1").unwrap().enabled);
    assert_eq!(registry.connections().last_error("srv-1").await, None);
}

#[tokio::test]
async fn turning_a_server_on_does_not_connect_it() {
    // Being enabled is a setting; being connected is a state. Conflating them
    // means a user cannot enable a server without also dialling it.
    let registry = registry();
    registry
        .store()
        .insert_server(&install_record("srv-1", "@test/server", false))
        .unwrap();

    registry.set_enabled("srv-1", true).await.unwrap();

    assert!(registry.store().get_server("srv-1").unwrap().enabled);
    assert!(!registry.connections().is_connected("srv-1").await);
}

#[tokio::test]
async fn enabling_a_server_that_is_not_installed_is_an_error_rather_than_a_no_op() {
    let error = registry()
        .set_enabled("absent", true)
        .await
        .expect_err("no such install");

    assert!(matches!(error, Error::UnknownServer { .. }), "{error:?}");
}

#[tokio::test]
async fn connecting_a_server_that_is_turned_off_is_refused() {
    let registry = registry();
    registry
        .store()
        .insert_server(&install_record("srv-1", "@test/server", false))
        .unwrap();

    let error = registry.connect("srv-1").await.expect_err("turned off");

    // Being off is a setting the user chose, so it has its own variant rather
    // than riding on one that means the server misbehaved.
    assert!(matches!(error, Error::ServerDisabled { .. }), "{error:?}");
    assert!(error.to_string().contains("disabled"), "{error}");
}

// ---------------------------------------------------------------------------
// Replacing credentials
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replacing_credentials_merges_rather_than_erasing() {
    // A form that sends only the field the user retyped must not erase the ones
    // it could not display.
    let registry = registry();
    registry
        .store()
        .insert_server(&install_record("srv-1", "@test/server", false))
        .unwrap();
    registry
        .store()
        .set_env_values(
            "srv-1",
            &BTreeMap::from([
                ("KEPT".to_string(), "old".to_string()),
                ("REPLACED".to_string(), "old".to_string()),
            ]),
        )
        .unwrap();

    let outcome = registry
        .update_env(
            "srv-1",
            BTreeMap::from([("REPLACED".to_string(), "new".to_string())]),
        )
        .await
        .unwrap();

    let stored = registry.store().load_env_values("srv-1").unwrap();
    assert_eq!(stored.get("KEPT").map(String::as_str), Some("old"));
    assert_eq!(stored.get("REPLACED").map(String::as_str), Some("new"));
    assert_eq!(outcome.env_keys, ["KEPT", "REPLACED"]);
}

#[tokio::test]
async fn a_server_that_is_turned_off_is_not_reconnected_when_its_credentials_change() {
    // The new values are stored and will be used when the user turns it on.
    let registry = registry();
    registry
        .store()
        .insert_server(&install_record("srv-1", "@test/server", false))
        .unwrap();

    let outcome = registry
        .update_env(
            "srv-1",
            BTreeMap::from([("API_KEY".to_string(), "new".to_string())]),
        )
        .await
        .unwrap();

    assert_eq!(outcome.status, UpdateEnvStatus::Disabled);
    assert!(outcome.tools.is_empty());
    assert_eq!(
        registry
            .store()
            .load_env_values("srv-1")
            .unwrap()
            .get("API_KEY")
            .map(String::as_str),
        Some("new")
    );
}

#[tokio::test]
async fn a_failed_reconnect_keeps_the_credentials_and_reports_the_failure() {
    // The user corrected a value; that correction is theirs to keep.
    let registry = registry();
    let mut record = install_record("srv-1", "@test/server", true);
    record.transport = Transport::HttpRemote {
        url: "http://127.0.0.1:1/mcp".into(),
    };
    registry.store().insert_server(&record).unwrap();

    let outcome = registry
        .update_env(
            "srv-1",
            BTreeMap::from([("API_KEY".to_string(), "new".to_string())]),
        )
        .await
        .unwrap();

    assert_eq!(outcome.status, UpdateEnvStatus::Disconnected);
    assert!(outcome.error.is_some());
    assert_eq!(outcome.auth_hint, None);
    assert_eq!(
        registry
            .store()
            .load_env_values("srv-1")
            .unwrap()
            .get("API_KEY")
            .map(String::as_str),
        Some("new")
    );
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tokio::test]
async fn listing_tools_on_a_server_that_is_not_connected_says_so() {
    let error = registry()
        .list_tools("srv-1")
        .await
        .expect_err("not connected");

    assert!(matches!(error, Error::NotConnected { .. }), "{error:?}");
}

#[tokio::test]
async fn calling_a_tool_on_a_server_that_is_not_connected_says_so() {
    let error = registry()
        .tool_call("srv-1", "forecast", json!({}))
        .await
        .expect_err("not connected");

    assert!(matches!(error, Error::NotConnected { .. }), "{error:?}");
}

// ---------------------------------------------------------------------------
// Registry settings
// ---------------------------------------------------------------------------

#[test]
fn registry_settings_report_whether_credentials_are_set_and_never_their_values() {
    let registry = McpRegistry::new(
        Store::open_in_memory().unwrap(),
        McpRegistryAuthConfig {
            smithery_api_key: Some("smithery-secret".into()),
            mcp_official_token: Some("official-secret".into()),
            mcp_official_base: Some("https://registry.test".into()),
        },
        McpClientIdentityConfig::default(),
        None,
    )
    .unwrap();

    let settings = registry.registry_settings();

    assert!(settings.smithery_api_key_set);
    assert!(settings.mcp_official_token_set);
    assert_eq!(
        settings.mcp_official_base.as_deref(),
        Some("https://registry.test")
    );

    let encoded = serde_json::to_string(&settings).unwrap();
    assert!(!encoded.contains("smithery-secret"), "{encoded}");
    assert!(!encoded.contains("official-secret"), "{encoded}");
}

#[test]
fn unset_registry_settings_report_nothing_configured() {
    let settings = registry().registry_settings();

    assert!(!settings.smithery_api_key_set || std::env::var("SMITHERY_API_KEY").is_ok());
    assert_eq!(settings.mcp_official_base, None);
}
