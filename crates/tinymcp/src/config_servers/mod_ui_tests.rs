//! Tests for the static registry's MCP Apps surface: tool `_meta` lookups and
//! resources, over both transports.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;
use tinymcp_bus::{McpClientConfig, McpClientIdentityConfig, McpServerConfig};

use super::McpServerRegistry;
use crate::Error;
use crate::transport::ui_fixture::{self, APP_MIME, CARD_HTML, CARD_URI, show_card_meta};

fn registry_for(server: McpServerConfig, identity: McpClientIdentityConfig) -> McpServerRegistry {
    McpServerRegistry::from_config(&McpClientConfig {
        servers: vec![server],
        client_identity: identity,
        ..McpClientConfig::default()
    })
    .unwrap()
}

fn http(endpoint: &str, disallowed: &[&str]) -> McpServerConfig {
    McpServerConfig {
        name: "ui".into(),
        endpoint: endpoint.into(),
        disallowed_tools: disallowed.iter().map(ToString::to_string).collect(),
        ..McpServerConfig::default()
    }
}

#[tokio::test]
async fn tool_meta_is_answered_from_the_last_listing() {
    let fixture = ui_fixture::spawn().await;
    let registry = registry_for(
        http(&fixture.endpoint, &[]),
        McpClientIdentityConfig::default(),
    );

    assert_eq!(registry.tool_meta("ui", "show_card"), None);
    let tools = registry.list_tools("ui").await.unwrap();
    assert_eq!(
        tools.iter().find(|t| t.name == "show_card").unwrap().meta,
        Some(show_card_meta())
    );

    assert_eq!(
        registry.tool_meta("ui", "show_card"),
        Some(show_card_meta())
    );
    assert_eq!(registry.tool_meta("ui", "plain"), None);
    assert_eq!(registry.tool_meta("nope", "show_card"), None);
}

#[tokio::test]
async fn tool_meta_is_withheld_for_a_disallowed_tool() {
    let fixture = ui_fixture::spawn().await;
    let registry = registry_for(
        http(&fixture.endpoint, &["show_card"]),
        McpClientIdentityConfig::default(),
    );
    registry.initialize("ui").await.unwrap();
    let _ = registry.list_tools("ui").await.unwrap();

    assert_eq!(registry.tool_meta("ui", "show_card"), None);
}

#[tokio::test]
async fn the_configured_capabilities_reach_the_server() {
    let fixture = ui_fixture::spawn().await;
    let capabilities = json!({
        "extensions": { "io.modelcontextprotocol/ui": { "mimeTypes": [APP_MIME] } }
    });
    let registry = registry_for(
        http(&fixture.endpoint, &[]),
        McpClientIdentityConfig {
            capabilities: capabilities.clone(),
            ..McpClientIdentityConfig::default()
        },
    );

    registry.initialize("ui").await.unwrap();

    assert_eq!(
        fixture.initialize_params.lock().unwrap()[0]["capabilities"],
        capabilities
    );
}

#[tokio::test]
async fn resources_are_listed_and_read_by_server_name() {
    let fixture = ui_fixture::spawn().await;
    let registry = registry_for(
        http(&fixture.endpoint, &[]),
        McpClientIdentityConfig::default(),
    );

    let listed = registry.list_resources("ui").await.unwrap();
    assert_eq!(listed.len(), 2);
    let contents = registry.read_resource("ui", CARD_URI).await.unwrap();
    assert_eq!(contents[0].text.as_deref(), Some(CARD_HTML));
}

#[tokio::test]
async fn resources_of_an_unknown_server_are_refused() {
    let registry = McpServerRegistry::default();

    let error = registry.read_resource("nope", CARD_URI).await.unwrap_err();
    assert!(matches!(error, Error::UnknownServer { .. }), "{error:?}");
    let error = registry.list_resources("nope").await.unwrap_err();
    assert!(matches!(error, Error::UnknownServer { .. }), "{error:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_stdio_server_answers_meta_and_resources_too() {
    use std::fmt::Write as _;

    let initialize = format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"{}","capabilities":{{}},"serverInfo":{{}}}}}}"#,
        tinymcp_bus::LATEST_PROTOCOL_VERSION
    );
    let replies = [
        initialize.as_str(),
        "",
        r#"{"jsonrpc":"2.0","id":3,"result":{"tools":[{"name":"show_card","_meta":{"ui":{"resourceUri":"ui://card"}}}]}}"#,
        r#"{"jsonrpc":"2.0","id":4,"result":{"resources":[{"uri":"ui://card","name":"card"}]}}"#,
        r#"{"jsonrpc":"2.0","id":5,"result":{"contents":[{"uri":"ui://card","text":"<p>stdio</p>"}]}}"#,
    ];
    let mut script = String::new();
    for reply in replies {
        script.push_str("read -r _line\n");
        let _ = writeln!(script, "printf '%s\\n' '{reply}'");
    }
    script.push_str("cat > /dev/null\n");
    let registry = registry_for(
        McpServerConfig {
            name: "ui".into(),
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script],
            ..McpServerConfig::default()
        },
        McpClientIdentityConfig::default(),
    );

    let _ = registry.list_tools("ui").await.unwrap();
    assert_eq!(
        registry.tool_meta("ui", "show_card"),
        Some(show_card_meta())
    );
    assert_eq!(
        registry.list_resources("ui").await.unwrap()[0].uri,
        CARD_URI
    );
    let contents = registry.read_resource("ui", CARD_URI).await.unwrap();
    assert_eq!(contents[0].text.as_deref(), Some("<p>stdio</p>"));
}
