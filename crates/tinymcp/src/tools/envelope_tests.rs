//! Tests for the `mcp_result` envelope tool results carry as host-only
//! metadata: what it holds, that the model-facing content is unchanged by it,
//! that secrets are scrubbed from it, and that a host reading call outcomes
//! with [`McpCallOutcome::from_metadata`] still finds them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use serde_json::{Value, json};
use tinymcp_bus::{
    ConnectedServerOverview, MCP_RESULT_KIND, McpAuthConfig, McpCallOutcome, McpClientConfig,
    McpResultEnvelope, McpServerConfig, McpTool, McpToolResult,
};
use tinytools::{Tool, ToolResult};

use super::{
    ActGate, McpCallTool, McpToolInvoker, McpToolSource, tool_result, tool_result_for, tools_for,
};
use crate::McpServerRegistry;
use crate::transport::render_tool_result;
use crate::transport::ui_fixture::{self, CALL_TEXT, CARD_HTML, CARD_URI};

fn envelope_of(result: &ToolResult) -> McpResultEnvelope {
    McpResultEnvelope::from_metadata(result.metadata.as_ref().expect("metadata is attached"))
        .expect("metadata decodes as an envelope")
}

#[test]
fn tool_result_for_attaches_the_envelope_and_leaves_content_alone() {
    let rendered = render_tool_result(&ui_fixture::call_reply());

    let plain = tool_result(rendered.clone());
    let enveloped = tool_result_for("srv", "show_card", rendered);

    assert_eq!(
        serde_json::to_value(&plain.content).unwrap(),
        serde_json::to_value(&enveloped.content).unwrap()
    );
    assert_eq!(plain.output(), enveloped.output());
    assert_eq!(plain.markdown_formatted, enveloped.markdown_formatted);
    assert!(plain.metadata.is_none());

    let envelope = envelope_of(&enveloped);
    assert_eq!(envelope.kind, MCP_RESULT_KIND);
    assert_eq!(envelope.server, "srv");
    assert_eq!(envelope.tool, "show_card");
    assert_eq!(
        envelope.structured_content,
        Some(json!({ "temperature": 21 }))
    );
    assert_eq!(envelope.resources[0].text.as_deref(), Some(CARD_HTML));
    assert_eq!(envelope.outcome, None);
    assert_eq!(
        McpCallOutcome::from_metadata(enveloped.metadata.as_ref().unwrap()),
        None
    );
}

#[test]
fn a_plain_result_carries_a_bare_envelope() {
    let result = tool_result_for("srv", "t", McpToolResult::success("hi"));

    assert_eq!(
        result.metadata,
        Some(json!({ "kind": MCP_RESULT_KIND, "server": "srv", "tool": "t" }))
    );
}

#[derive(Debug)]
struct Fixed;

#[async_trait::async_trait]
impl McpToolInvoker for Fixed {
    async fn invoke(&self, _: &str, _: &str, _: Value) -> crate::Result<McpToolResult> {
        Ok(render_tool_result(&ui_fixture::call_reply()))
    }
}

#[tokio::test]
async fn a_server_tool_returns_the_envelope_for_its_server_and_remote_name() {
    let invoker: Arc<dyn McpToolInvoker> = Arc::new(Fixed);
    let overview = ConnectedServerOverview {
        server_id: "srv-1".into(),
        qualified_name: "@acme/cards".into(),
        display_name: "Cards".into(),
        description: None,
        instructions: None,
        tools: vec![McpTool::new("show_card")],
    };
    let tool = tools_for(&[McpToolSource::from_overview(&overview)], &invoker).remove(0);

    let result = tool.execute(json!({})).await.unwrap();

    assert_eq!(result.output(), CALL_TEXT);
    let envelope = envelope_of(&result);
    assert_eq!(envelope.server, "srv-1");
    assert_eq!(envelope.tool, "show_card");
    assert_eq!(
        envelope.meta,
        Some(json!({ "openai/outputTemplate": CARD_URI }))
    );
}

fn bridge_for(endpoint: &str, auth: McpAuthConfig) -> McpCallTool {
    let registry = McpServerRegistry::from_config(&McpClientConfig {
        servers: vec![McpServerConfig {
            name: "ui".into(),
            endpoint: endpoint.into(),
            auth,
            ..McpServerConfig::default()
        }],
        ..McpClientConfig::default()
    })
    .unwrap();
    let gate: ActGate = Arc::new(|_| Ok(()));
    McpCallTool::new(Arc::new(registry), gate)
}

#[tokio::test]
async fn an_answered_bridge_call_carries_the_envelope_with_its_outcome() {
    let fixture = ui_fixture::spawn().await;
    let result = bridge_for(&fixture.endpoint, McpAuthConfig::None)
        .execute(json!({ "server": "ui", "tool": "show_card", "arguments": {} }))
        .await
        .unwrap();

    let envelope = envelope_of(&result);
    assert_eq!(envelope.server, "ui");
    assert_eq!(envelope.tool, "show_card");
    assert_eq!(
        envelope.structured_content,
        Some(json!({ "temperature": 21 }))
    );
    assert_eq!(envelope.resources[0].uri, CARD_URI);
    assert_eq!(
        envelope.outcome,
        Some(McpCallOutcome::answered("ui", "show_card"))
    );
    assert_eq!(
        McpCallOutcome::from_metadata(result.metadata.as_ref().unwrap()),
        Some(McpCallOutcome::answered("ui", "show_card"))
    );
    assert_eq!(result.output(), CALL_TEXT);
    assert!(!result.output().contains("temperature"));
}

#[tokio::test]
async fn secrets_are_scrubbed_from_the_envelope() {
    const SECRET: &str = "envelope-Secret_42";
    let fixture = ui_fixture::spawn().await;
    let tool = bridge_for(
        &fixture.endpoint,
        McpAuthConfig::BearerToken {
            token: SECRET.into(),
        },
    );

    let result = tool
        .execute(json!({ "server": "ui", "tool": "show_card", "arguments": {} }))
        .await
        .unwrap();
    let mut envelope = envelope_of(&result);
    envelope.structured_content = Some(json!({ "echo": SECRET }));
    envelope.meta = Some(json!({ "echo": SECRET }));
    envelope.resources[0].text = Some(format!("<p>{SECRET}</p>"));
    envelope.resources[0].meta = Some(json!({ "echo": SECRET }));

    let scrubber = super::SecretScrubber::for_server(
        &McpServerRegistry::from_config(&McpClientConfig {
            servers: vec![McpServerConfig {
                name: "ui".into(),
                endpoint: fixture.endpoint.clone(),
                auth: McpAuthConfig::BearerToken {
                    token: SECRET.into(),
                },
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        })
        .unwrap(),
        "ui",
    );
    let cleaned = super::bridge::scrubbed_envelope(&scrubber, envelope).to_metadata();

    assert!(!cleaned.to_string().contains(SECRET), "{cleaned}");
    assert_eq!(cleaned["kind"], MCP_RESULT_KIND);
    assert_eq!(cleaned["outcome"]["ok"], true);
}
