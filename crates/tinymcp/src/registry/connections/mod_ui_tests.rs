//! Tests for the connection map's MCP Apps surface: tool `_meta` captured at
//! connect, and resources read through a connected server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;
use tinymcp_bus::{CommandKind, InstalledServer, McpClientIdentityConfig, Transport};

use super::types::Connections;
use crate::Error;
use crate::registry::Store;
use crate::registry::oauth::OAuthFlow;
use crate::transport::ui_fixture::{self, APP_MIME, CARD_HTML, CARD_URI, show_card_meta};

fn install(url: &str) -> InstalledServer {
    InstalledServer {
        server_id: "ui".into(),
        qualified_name: "@test/ui".into(),
        display_name: "ui".into(),
        description: None,
        icon_url: None,
        command_kind: CommandKind::Node,
        command: "npx".into(),
        args: Vec::new(),
        env_keys: Vec::new(),
        config: None,
        installed_at: 1_000,
        last_connected_at: None,
        transport: Transport::HttpRemote {
            url: url.to_string(),
        },
        enabled: true,
    }
}

async fn connected(identity: &McpClientIdentityConfig) -> (Connections, ui_fixture::UiFixture) {
    let fixture = ui_fixture::spawn().await;
    let server = install(&fixture.endpoint);
    let store = Store::open_in_memory().unwrap();
    store.insert_server(&server).unwrap();
    let connections = Connections::new();
    connections
        .connect(
            &store,
            &OAuthFlow::new(None).unwrap(),
            identity,
            None,
            &server,
        )
        .await
        .unwrap();
    (connections, fixture)
}

#[tokio::test]
async fn tool_meta_is_captured_at_connect() {
    let (connections, _fixture) = connected(&McpClientIdentityConfig::default()).await;

    assert_eq!(
        connections.tool_meta("ui", "show_card").await,
        Some(show_card_meta())
    );
    assert_eq!(connections.tool_meta("ui", "plain").await, None);
    assert_eq!(connections.tool_meta("ui", "missing").await, None);
    assert_eq!(connections.tool_meta("other", "show_card").await, None);

    let metas = connections.tool_metas("ui").await.unwrap();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas.get("show_card"), Some(&show_card_meta()));
    assert!(connections.tool_metas("other").await.is_none());
}

#[tokio::test]
async fn the_identity_capabilities_reach_the_server_on_connect() {
    let capabilities = json!({
        "extensions": { "io.modelcontextprotocol/ui": { "mimeTypes": [APP_MIME] } }
    });
    let identity = McpClientIdentityConfig {
        capabilities: capabilities.clone(),
        ..McpClientIdentityConfig::default()
    };
    let (_connections, fixture) = connected(&identity).await;

    assert_eq!(
        fixture.initialize_params.lock().unwrap()[0]["capabilities"],
        capabilities
    );
}

#[tokio::test]
async fn a_connected_servers_resources_are_listed_and_read() {
    let (connections, _fixture) = connected(&McpClientIdentityConfig::default()).await;

    let listed = connections.list_resources("ui").await.unwrap();
    assert_eq!(listed[0].uri, CARD_URI);

    let contents = connections.read_resource("ui", CARD_URI).await.unwrap();
    assert_eq!(contents[0].text.as_deref(), Some(CARD_HTML));
}

#[tokio::test]
async fn resources_of_an_unconnected_server_are_refused() {
    let connections = Connections::new();

    let error = connections.read_resource("ui", CARD_URI).await.unwrap_err();
    assert!(matches!(error, Error::NotConnected { .. }), "{error:?}");
    let error = connections.list_resources("ui").await.unwrap_err();
    assert!(matches!(error, Error::NotConnected { .. }), "{error:?}");
}
