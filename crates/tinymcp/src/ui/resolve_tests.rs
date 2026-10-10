//! Unit tests for template resolution, call views and widget documents.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::json;

fn view() -> UiCallView {
    UiCallView {
        server_id: "srv".into(),
        tool: "order".into(),
        tool_input: json!({"item": "dosa"}),
        ..UiCallView::default()
    }
}

#[test]
fn resource_uri_precedence() {
    let mut v = view();
    v.tool_meta = Some(json!({
        "openai/outputTemplate": "ui://widget/sdk.html",
        "ui/resourceUri": "ui://widget/legacy.html",
        "ui": {"resourceUri": "ui://widget/app.html"}
    }));
    assert_eq!(
        select_source(&v),
        Some(UiSource::Uri {
            uri: "ui://widget/app.html".into(),
            flavor: UiFlavor::McpApps
        })
    );

    v.tool_meta = Some(json!({
        "openai/outputTemplate": "ui://widget/sdk.html",
        "ui/resourceUri": "ui://widget/legacy.html"
    }));
    assert_eq!(
        select_source(&v),
        Some(UiSource::Uri {
            uri: "ui://widget/legacy.html".into(),
            flavor: UiFlavor::McpApps
        })
    );

    v.tool_meta = Some(json!({"openai/outputTemplate": "ui://widget/sdk.html"}));
    assert_eq!(
        select_source(&v),
        Some(UiSource::Uri {
            uri: "ui://widget/sdk.html".into(),
            flavor: UiFlavor::AppsSdk
        })
    );
}

#[test]
fn only_ui_scheme_is_accepted() {
    let mut v = view();
    v.tool_meta = Some(json!({"ui": {"resourceUri": "https://evil.example.com/w.html"}}));
    assert_eq!(select_source(&v), None);
    v.tool_meta = Some(json!({"ui": {"resourceUri": "file:///etc/passwd"}}));
    assert_eq!(select_source(&v), None);
    assert!(!is_ui_uri("ui://"));
    assert!(is_ui_uri("UI://x"));
    assert!(!is_ui_uri("é://xx"));
}

#[test]
fn embedded_html_resource_is_returned_as_an_inline_document() {
    let mut v = view();
    v.resources = vec![
        json!({"uri": "ui://w/plain", "mimeType": "text/plain", "text": "x"}),
        json!({"uri": "https://x/y", "mimeType": "text/html", "text": "<p>no</p>"}),
        json!({"uri": "ui://w/card", "mimeType": "text/html;profile=mcp-app", "text": "<p>hi</p>"}),
    ];
    let resolved = resolve_presentation(&v).expect("presentation");
    assert_eq!(resolved.presentation.kind, MCP_UI_KIND);
    assert!(resolved.presentation.resource_uri.is_none());
    assert!(resolved.presentation.inline_id.is_none());
    assert_eq!(
        resolved.inline_document.expect("document").html,
        "<p>hi</p>"
    );
    assert!(resolved.prefetched.is_none());
}

#[test]
fn a_template_document_the_result_embedded_is_prefetched() {
    let mut v = view();
    v.tool_meta = Some(json!({"ui": {"resourceUri": "ui://w/card"}}));
    v.resources =
        vec![json!({"uri": "ui://w/card", "mimeType": "text/html", "text": "<p>card</p>"})];
    let resolved = resolve_presentation(&v).expect("presentation");
    assert_eq!(
        resolved.presentation.resource_uri.as_deref(),
        Some("ui://w/card")
    );
    let (uri, document) = resolved.prefetched.expect("prefetched");
    assert_eq!(uri, "ui://w/card");
    assert_eq!(document.html, "<p>card</p>");
    assert!(resolved.inline_document.is_none());
}

#[test]
fn template_from_meta_follows_precedence_and_scheme() {
    assert_eq!(
        template_from_meta(
            &json!({"openai/outputTemplate": "ui://a", "ui": {"resourceUri": "ui://b"}})
        ),
        Some(("ui://b".to_string(), UiFlavor::McpApps))
    );
    assert_eq!(
        template_from_meta(&json!({"openai/outputTemplate": " ui://a "})),
        Some(("ui://a".to_string(), UiFlavor::AppsSdk))
    );
    assert_eq!(
        template_from_meta(&json!({"ui": {"resourceUri": "https://x"}})),
        None
    );
    assert_eq!(template_from_meta(&json!({})), None);
}

#[test]
fn no_ui_and_no_links_yields_nothing() {
    let mut v = view();
    v.text = "Your order is placed.".into();
    assert!(resolve_presentation(&v).is_none());
}

#[test]
fn links_only_presentation() {
    let mut v = view();
    v.text = "Pay here: upi://pay?pa=shop@bank&am=120".into();
    let presentation = resolve_presentation(&v).expect("presentation").presentation;
    assert!(!presentation.has_frame());
    assert_eq!(presentation.links.len(), 1);
    assert_eq!(presentation.tool_input, json!({"item": "dosa"}));
}

#[test]
fn oversized_structured_content_is_dropped_but_links_kept() {
    let mut v = view();
    let big = "x".repeat(MAX_STRUCTURED_BYTES + 10);
    v.structured_content = Some(json!({"blob": big, "pay": "https://pay.example.com/1"}));
    let presentation = resolve_presentation(&v).expect("presentation").presentation;
    assert!(presentation.structured_content.is_none());
    assert_eq!(presentation.links[0].url, "https://pay.example.com/1");
}

#[test]
fn raw_result_view_reads_content_blocks() {
    let raw = json!({
        "content": [
            {"type": "text", "text": "done"},
            {"type": "resource", "resource": {"uri": "ui://a", "mimeType": "text/html", "text": "<b/>"}}
        ],
        "structuredContent": {"id": 1},
        "_meta": {"k": "v"}
    });
    let v = view_from_raw_result("srv", "t", None, json!({}), &raw);
    assert_eq!(v.text, "done");
    assert_eq!(v.resources.len(), 1);
    assert_eq!(v.structured_content, Some(json!({"id": 1})));
    assert_eq!(v.result_meta, Some(json!({"k": "v"})));
}

#[test]
fn envelope_view_requires_mcp_result_kind() {
    assert!(view_from_envelope(&json!({"kind": "mcp_call"}), None, json!({}), "").is_none());
    let envelope = json!({
        "kind": "mcp_result",
        "server": "srv",
        "tool": "t",
        "structured_content": {"a": 1},
        "meta": null,
        "resources": [{"uri": "ui://x", "mimeType": "text/html", "text": "<i/>"}]
    });
    let v = view_from_envelope(&envelope, None, json!({}), "txt").expect("view");
    assert_eq!(v.server_id, "srv");
    assert!(v.result_meta.is_none());
    assert_eq!(v.resources.len(), 1);
    assert_eq!(v.text, "txt");
}

#[test]
fn resource_contents_validation() {
    let ok = json!({
        "uri": "ui://a",
        "mimeType": "text/html;profile=mcp-app",
        "text": "<html></html>",
        "_meta": {"ui": {
            "csp": {"connectDomains": ["https://api.example.com/x", "http://insecure.example.com", "javascript:alert(1)"],
                    "resourceDomains": ["https://*.cdn.example.com", "https://cdn.example.com:8443"]},
            "prefersBorder": true,
            "permissions": {"clipboardWrite": {}}
        }}
    });
    let resource = resource_from_contents(&[ok], "ui://a").expect("resource");
    assert_eq!(
        resource.csp.connect_domains,
        vec!["https://api.example.com"]
    );
    assert_eq!(
        resource.csp.resource_domains,
        vec!["https://*.cdn.example.com", "https://cdn.example.com:8443"]
    );
    assert!(resource.prefers_border);
    assert!(resource.permissions.is_some());

    let not_html = json!({"uri": "ui://a", "mimeType": "application/javascript", "text": "x"});
    assert!(resource_from_contents(&[not_html], "ui://a").is_err());

    let huge =
        json!({"uri": "ui://a", "mimeType": "text/html", "text": "x".repeat(MAX_WIDGET_BYTES + 1)});
    assert!(resource_from_contents(&[huge], "ui://a").is_err());

    let blob = json!({"uri": "ui://a", "mimeType": "text/html+skybridge", "blob": "PGI+aGk8L2I+",
                      "_meta": {"openai/widgetCSP": {"connect_domains": ["https://api.example.com"]},
                                "openai/widgetPrefersBorder": true}});
    let resource = resource_from_contents(&[blob], "ui://a").expect("blob");
    assert_eq!(resource.html, "<b>hi</b>");
    assert_eq!(
        resource.csp.connect_domains,
        vec!["https://api.example.com"]
    );
    assert!(resource.prefers_border);
}

#[test]
fn https_origin_rejects_odd_hosts() {
    let origins = https_origins(Some(&json!([
        "https://localhost",
        "https://a..b.com",
        "https://ok.example.com",
        "https://ok.example.com",
        "https://x.example.com:abc",
        "https://user@x.example.com"
    ])));
    assert_eq!(origins, vec!["https://ok.example.com"]);
}
