//! Tests for the subprocess transport's MCP Apps surface: client capabilities
//! at `initialize`, tool `_meta` kept from `tools/list`, and resources.
//!
//! Unix-only for the reason the main suite gives: the fake server is a shell
//! script.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;

use serde_json::json;
use tinymcp_bus::{LATEST_PROTOCOL_VERSION, McpClientIdentityConfig};

use super::McpStdioClient;
use crate::Error;

fn initialize_reply() -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"{LATEST_PROTOCOL_VERSION}","capabilities":{{}},"serverInfo":{{"name":"fake","version":"1"}}}}}}"#
    )
}

/// A server answering each request in order, one JSON line per reply.
fn responder(replies: &[&str]) -> String {
    let mut body = String::new();
    for reply in replies {
        body.push_str("read -r _line\n");
        let _ = writeln!(body, "printf '%s\\n' '{reply}'");
    }
    body.push_str("cat > /dev/null\n");
    body
}

/// A server that completes the handshake only when the `initialize` line
/// contains `expected`, and answers an RPC error otherwise.
fn capability_checker(expected: &str) -> String {
    let error =
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"unexpected capabilities"}}"#;
    format!(
        "read -r line\ncase \"$line\" in *'{expected}'*) printf '%s\\n' '{}' ;; *) printf '%s\\n' '{error}' ;; esac\ncat > /dev/null\n",
        initialize_reply()
    )
}

fn client_with(body: String, identity: &McpClientIdentityConfig) -> McpStdioClient {
    McpStdioClient::new(
        "/bin/sh",
        vec!["-c".to_string(), body],
        Vec::new(),
        None,
        identity,
    )
}

fn client_for(body: String) -> McpStdioClient {
    client_with(body, &McpClientIdentityConfig::default())
}

#[tokio::test]
async fn initialize_sends_empty_capabilities_by_default() {
    let client = client_for(capability_checker(r#""capabilities":{}"#));

    client.initialize().await.expect("the default capabilities");
}

#[tokio::test]
async fn initialize_sends_the_capabilities_the_identity_carries() {
    let identity = McpClientIdentityConfig {
        capabilities: json!({ "extensions": { "x": {} } }),
        ..McpClientIdentityConfig::default()
    };
    let client = client_with(
        capability_checker(r#""capabilities":{"extensions":{"x":{}}}"#),
        &identity,
    );

    client
        .initialize()
        .await
        .expect("the configured capabilities");
}

#[tokio::test]
async fn with_capabilities_overrides_the_identity() {
    let client = client_for(capability_checker(
        r#""capabilities":{"extensions":{"y":{}}}"#,
    ))
    .with_capabilities(json!({ "extensions": { "y": {} } }));

    client
        .initialize()
        .await
        .expect("the overriding capabilities");
}

#[tokio::test]
async fn capabilities_the_server_did_not_expect_fail_the_check() {
    let client = client_for(capability_checker(r#""capabilities":{"extensions""#));

    let error = client.initialize().await.unwrap_err();

    assert!(matches!(error, Error::Rpc { .. }), "{error:?}");
}

#[tokio::test]
async fn tools_list_keeps_meta_and_caches_it() {
    let tools = r#"{"jsonrpc":"2.0","id":3,"result":{"tools":[{"name":"show_card","_meta":{"ui":{"resourceUri":"ui://card"}}},{"name":"plain"}]}}"#;
    let client = client_for(responder(&[&initialize_reply(), "", tools]));

    assert!(client.cached_tool("show_card").is_none());
    let listed = client.list_tools().await.unwrap();

    assert_eq!(
        listed[0].meta,
        Some(json!({ "ui": { "resourceUri": "ui://card" } }))
    );
    assert_eq!(
        client.cached_tool("show_card").unwrap().meta,
        Some(json!({ "ui": { "resourceUri": "ui://card" } }))
    );
    assert_eq!(client.cached_tool("plain").unwrap().meta, None);
}

#[tokio::test]
async fn resources_are_listed_across_pages() {
    let first = r#"{"jsonrpc":"2.0","id":3,"result":{"resources":[{"uri":"ui://a","name":"a"}],"nextCursor":"c2"}}"#;
    let second = r#"{"jsonrpc":"2.0","id":4,"result":{"resources":[{"uri":"ui://b","name":"b"}]}}"#;
    let client = client_for(responder(&[&initialize_reply(), "", first, second]));

    let resources = client.list_resources().await.unwrap();

    let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(uris, ["ui://a", "ui://b"]);
}

#[tokio::test]
async fn a_resource_is_read_verbatim() {
    let read = r#"{"jsonrpc":"2.0","id":3,"result":{"contents":[{"uri":"ui://a","mimeType":"text/html;profile=mcp-app","text":"<p>hi</p>"}]}}"#;
    let client = client_for(responder(&[&initialize_reply(), "", read]));

    let contents = client.read_resource("ui://a").await.unwrap();

    assert_eq!(contents[0].text.as_deref(), Some("<p>hi</p>"));
    assert_eq!(
        contents[0].mime_type.as_deref(),
        Some("text/html;profile=mcp-app")
    );
}

#[tokio::test]
async fn a_read_reply_without_contents_is_malformed() {
    let read = r#"{"jsonrpc":"2.0","id":3,"result":{}}"#;
    let client = client_for(responder(&[&initialize_reply(), "", read]));

    let error = client.read_resource("ui://a").await.unwrap_err();

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_tool_call_keeps_structured_content() {
    let call = r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"ok"}],"structuredContent":{"n":1}}}"#;
    let client = client_for(responder(&[&initialize_reply(), "", call]));

    let result = client.call_tool("show_card", json!({})).await.unwrap();

    assert_eq!(result.rendered.text(), "ok");
    assert_eq!(result.rendered.structured_content, Some(json!({ "n": 1 })));
}
