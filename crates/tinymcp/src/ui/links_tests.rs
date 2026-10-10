//! Unit tests for link extraction and classification.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::json;

fn urls(links: &[UiLink]) -> Vec<&str> {
    links.iter().map(|link| link.url.as_str()).collect()
}

#[test]
fn keeps_https_and_handoff_schemes() {
    let text =
        "Pay at https://pay.example.com/o/1 or phonepe://pay?id=1 or upi://pay?pa=a@b&am=10.";
    let links = extract_links(None, text);
    assert_eq!(
        urls(&links),
        vec![
            "https://pay.example.com/o/1",
            "phonepe://pay?id=1",
            "upi://pay?pa=a@b&am=10"
        ]
    );
    assert_eq!(links[0].kind, UiLinkKind::External);
    assert_eq!(links[1].kind, UiLinkKind::Handoff);
    assert_eq!(links[2].kind, UiLinkKind::Handoff);
}

#[test]
fn drops_dangerous_schemes() {
    let text = "javascript://x%0Aalert(1) data://text/html,hi file:///etc/passwd tauri://localhost ohwidget://x blob://y";
    assert_eq!(extract_links(None, text), Vec::new());
}

#[test]
fn classify_table() {
    assert_eq!(classify_link("https://a.com"), LinkClass::Web);
    assert_eq!(classify_link("http://a.com/x"), LinkClass::Web);
    assert_eq!(classify_link("tez://upi/pay"), LinkClass::Handoff);
    assert_eq!(
        classify_link("intent://pay#Intent;scheme=upi;end"),
        LinkClass::Handoff
    );
    assert_eq!(classify_link("https://a.com/p/photo.PNG"), LinkClass::Image);
    assert_eq!(classify_link("JAVASCRIPT:alert(1)"), LinkClass::Blocked);
    assert_eq!(classify_link("data:text/html,x"), LinkClass::Blocked);
    assert_eq!(classify_link("https://"), LinkClass::Blocked);
    assert_eq!(classify_link("nonsense"), LinkClass::Blocked);
    assert_eq!(classify_link("1http://a.com"), LinkClass::Blocked);
}

#[test]
fn only_web_and_handoff_are_actions() {
    assert_eq!(link_kind(LinkClass::Web), Some(UiLinkKind::External));
    assert_eq!(link_kind(LinkClass::Handoff), Some(UiLinkKind::Handoff));
    assert_eq!(link_kind(LinkClass::Image), None);
    assert_eq!(link_kind(LinkClass::Blocked), None);
}

#[test]
fn structured_content_comes_first_and_dedupes() {
    let structured = json!({
        "order": {"a_payment_url": "https://pay.example.com/o/1", "b_deep": ["phonepe://pay?id=1"]}
    });
    let links = extract_links(
        Some(&structured),
        "See https://pay.example.com/o/1 and https://other.example.com.",
    );
    assert_eq!(
        urls(&links),
        vec![
            "https://pay.example.com/o/1",
            "phonepe://pay?id=1",
            "https://other.example.com"
        ]
    );
}

#[test]
fn caps_at_max_links() {
    let text = (0..10)
        .map(|i| format!("https://e{i}.example.com"))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(extract_links(None, &text).len(), MAX_LINKS);
}

#[test]
fn trims_markdown_punctuation_and_unbalanced_parens() {
    let links = extract_links(
        None,
        "[pay](https://pay.example.com/x) (see https://a.example.com/p_(1)).",
    );
    assert_eq!(
        urls(&links),
        vec!["https://pay.example.com/x", "https://a.example.com/p_(1)"]
    );
}

#[test]
fn drops_image_assets() {
    let structured = json!({
        "a_image": "https://media.example.com/shop/image/upload/fl_lossy,f_auto/CATALOG/2026/6/4/x_1",
        "b_photo": "https://cdn.example.com/products/bar_1.JPG",
        "c_page": "https://www.example.com/shop/item/123",
    });
    let text = "Logo https://cdn.example.com/logo.svg?v=2 and pay at upi://pay?pa=a@b.png";
    let links = extract_links(Some(&structured), text);
    assert_eq!(
        urls(&links),
        vec![
            "https://www.example.com/shop/item/123",
            "upi://pay?pa=a@b.png"
        ]
    );
}

#[test]
fn image_url_table() {
    assert!(is_image_url(
        "https://res.cloudinary.com/demo/image/upload/sample"
    ));
    assert!(is_image_url("https://cdn.example.com/a/b.webp"));
    assert!(!is_image_url("https://example.com/pngs/list"));
    assert!(!is_image_url("https://example.com/checkout"));
    assert!(!is_image_url("not a url"));
}
