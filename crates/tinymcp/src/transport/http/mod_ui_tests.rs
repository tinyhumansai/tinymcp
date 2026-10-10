//! Tests for the HTTP transport's MCP Apps surface: client capabilities at
//! `initialize`, tool `_meta` kept from `tools/list`, and resources.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;
use tinymcp_bus::{MAX_RESOURCE_BYTES, McpClientIdentityConfig};

use super::McpHttpClient;
use crate::Error;
use crate::transport::ui_fixture::{
    self, APP_MIME, BIG_URI, CALL_TEXT, CARD_HTML, CARD_URI, show_card_meta,
};

#[tokio::test]
async fn initialize_sends_empty_capabilities_by_default() {
    let fixture = ui_fixture::spawn().await;
    let client = McpHttpClient::new(&fixture.endpoint, 5).unwrap();

    client.initialize().await.unwrap();

    let params = fixture.initialize_params.lock().unwrap().clone();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0]["capabilities"], json!({}));
}

#[tokio::test]
async fn initialize_sends_the_capabilities_the_identity_carries() {
    let fixture = ui_fixture::spawn().await;
    let capabilities = json!({
        "extensions": { "io.modelcontextprotocol/ui": { "mimeTypes": [APP_MIME] } }
    });
    let identity = McpClientIdentityConfig {
        capabilities: capabilities.clone(),
        ..McpClientIdentityConfig::default()
    };
    let client = McpHttpClient::builder(&fixture.endpoint)
        .identity(identity)
        .build()
        .unwrap();

    client.initialize().await.unwrap();

    assert_eq!(
        fixture.initialize_params.lock().unwrap()[0]["capabilities"],
        capabilities
    );
}

#[tokio::test]
async fn the_builder_setter_overrides_the_identity_capabilities() {
    let fixture = ui_fixture::spawn().await;
    let client = McpHttpClient::builder(&fixture.endpoint)
        .capabilities(json!({ "extensions": { "x": {} } }))
        .build()
        .unwrap();

    client.initialize().await.unwrap();

    assert_eq!(
        fixture.initialize_params.lock().unwrap()[0]["capabilities"],
        json!({ "extensions": { "x": {} } })
    );
}

#[tokio::test]
async fn discovery_sends_the_configured_capabilities_too() {
    let fixture = ui_fixture::spawn().await;
    let client = McpHttpClient::builder(&fixture.endpoint)
        .capabilities(json!({ "extensions": { "x": {} } }))
        .build()
        .unwrap();

    assert!(client.discover_authorization().await.unwrap().is_none());

    assert_eq!(
        fixture.initialize_params.lock().unwrap()[0]["capabilities"],
        json!({ "extensions": { "x": {} } })
    );
}

#[tokio::test]
async fn tools_list_keeps_each_tools_meta_and_caches_it() {
    let fixture = ui_fixture::spawn().await;
    let client = McpHttpClient::new(&fixture.endpoint, 5).unwrap();

    assert!(client.cached_tool("show_card").is_none());
    let tools = client.list_tools().await.unwrap();

    let card = tools.iter().find(|tool| tool.name == "show_card").unwrap();
    assert_eq!(card.meta, Some(show_card_meta()));
    let plain = tools.iter().find(|tool| tool.name == "plain").unwrap();
    assert_eq!(plain.meta, None);
    assert_eq!(
        client.cached_tool("show_card").unwrap().meta,
        Some(show_card_meta())
    );
    assert!(client.cached_tool("missing").is_none());
}

#[tokio::test]
async fn resources_are_listed_across_pages() {
    let fixture = ui_fixture::spawn().await;
    let client = McpHttpClient::new(&fixture.endpoint, 5).unwrap();

    let resources = client.list_resources().await.unwrap();

    let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(uris, [CARD_URI, "ui://other"]);
    assert_eq!(resources[0].mime_type.as_deref(), Some(APP_MIME));
}

#[tokio::test]
async fn a_resource_is_read_verbatim() {
    let fixture = ui_fixture::spawn().await;
    let client = McpHttpClient::new(&fixture.endpoint, 5).unwrap();

    let contents = client.read_resource(CARD_URI).await.unwrap();

    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].uri, CARD_URI);
    assert_eq!(contents[0].text.as_deref(), Some(CARD_HTML));
    assert_eq!(contents[0].mime_type.as_deref(), Some(APP_MIME));
}

#[tokio::test]
async fn an_oversized_resource_is_refused() {
    let fixture = ui_fixture::spawn().await;
    let client = McpHttpClient::new(&fixture.endpoint, 5).unwrap();

    let error = client.read_resource(BIG_URI).await.unwrap_err();

    match error {
        Error::ResourceTooLarge { uri, bytes, limit } => {
            assert_eq!(uri, BIG_URI);
            assert_eq!(bytes, MAX_RESOURCE_BYTES + 1);
            assert_eq!(limit, MAX_RESOURCE_BYTES);
        }
        other => panic!("expected ResourceTooLarge, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unknown_resource_is_an_rpc_error() {
    let fixture = ui_fixture::spawn().await;
    let client = McpHttpClient::new(&fixture.endpoint, 5).unwrap();

    let error = client.read_resource("ui://nope").await.unwrap_err();

    assert!(matches!(error, Error::Rpc { .. }), "{error:?}");
}

#[tokio::test]
async fn a_tool_call_keeps_structured_content_meta_and_resources() {
    let fixture = ui_fixture::spawn().await;
    let client = McpHttpClient::new(&fixture.endpoint, 5).unwrap();

    let result = client.call_tool("show_card", json!({})).await.unwrap();

    assert_eq!(result.rendered.text(), CALL_TEXT);
    assert_eq!(
        result.rendered.structured_content,
        Some(json!({ "temperature": 21 }))
    );
    assert_eq!(
        result.rendered.meta,
        Some(json!({ "openai/outputTemplate": CARD_URI }))
    );
    assert_eq!(result.rendered.resources.len(), 1);
    assert_eq!(
        result.rendered.resources[0].text.as_deref(),
        Some(CARD_HTML)
    );
}
