//! Unit tests for the widget tool-call policy and the rendering decision.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::ui::{MCP_UI_KIND, UiLink, UiLinkKind};

fn presentation(resource_uri: Option<&str>, links: Vec<UiLink>) -> McpUiPresentation {
    McpUiPresentation {
        kind: MCP_UI_KIND.to_string(),
        flavor: UiFlavor::McpApps,
        server_id: Some("srv".into()),
        tool: "t".into(),
        resource_uri: resource_uri.map(str::to_string),
        inline_id: None,
        title: None,
        tool_input: Value::Null,
        structured_content: None,
        result_meta: None,
        links,
    }
}

#[test]
fn visibility_defaults_to_app_and_honours_the_list() {
    assert!(visible_to_app(None));
    assert!(visible_to_app(Some(&json!({"ui": {"visibility": null}}))));
    assert!(visible_to_app(Some(
        &json!({"ui": {"visibility": ["model", "app"]}})
    )));
    assert!(!visible_to_app(Some(
        &json!({"ui": {"visibility": ["model"]}})
    )));
    assert!(!visible_to_app(Some(&json!({"ui": {"visibility": "app"}}))));
}

#[test]
fn read_only_needs_an_explicit_hint() {
    assert!(is_read_only(Some(&json!({"readOnlyHint": true}))));
    assert!(!is_read_only(Some(&json!({"readOnlyHint": false}))));
    assert!(!is_read_only(Some(&json!({"readOnlyHint": "true"}))));
    assert!(!is_read_only(None));
}

#[test]
fn widget_calls_confirm_unless_read_only_and_never_from_host_pages() {
    let read_only = json!({"readOnlyHint": true});
    assert_eq!(
        widget_call_policy(UiFlavor::McpApps, None, Some(&read_only)),
        WidgetCallPolicy::Allow
    );
    assert_eq!(
        widget_call_policy(UiFlavor::AppsSdk, None, None),
        WidgetCallPolicy::Confirm
    );
    assert_eq!(
        widget_call_policy(UiFlavor::HostInline, None, Some(&read_only)),
        WidgetCallPolicy::Deny
    );
    let hidden = json!({"ui": {"visibility": ["model"]}});
    assert_eq!(
        widget_call_policy(UiFlavor::McpApps, Some(&hidden), Some(&read_only)),
        WidgetCallPolicy::Deny
    );
}

#[test]
fn rendering_falls_back_to_links_without_widget_support() {
    let link = UiLink {
        url: "upi://pay".into(),
        kind: UiLinkKind::Handoff,
    };
    let both = presentation(Some("ui://w"), vec![link.clone()]);
    assert_eq!(rendering(true, &both), UiRendering::Widget);
    assert_eq!(rendering(false, &both), UiRendering::Links);
    assert_eq!(
        rendering(true, &presentation(None, vec![link])),
        UiRendering::Links
    );
    assert_eq!(
        rendering(false, &presentation(Some("ui://w"), Vec::new())),
        UiRendering::Nothing
    );
}

#[test]
fn capabilities_advertise_the_mcp_apps_extension() {
    assert_eq!(
        client_capabilities(),
        json!({"extensions": {"io.modelcontextprotocol/ui": {"mimeTypes": ["text/html;profile=mcp-app"]}}})
    );
}
