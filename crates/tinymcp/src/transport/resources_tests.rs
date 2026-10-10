//! Unit tests for decoding resource replies.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;
use tinymcp_bus::MAX_RESOURCE_BYTES;

use super::{list_params, parse_read_result, parse_resource_page};
use crate::Error;

#[test]
fn a_page_yields_its_resources_and_cursor() {
    let (resources, next) = parse_resource_page(&json!({
        "resources": [{ "uri": "ui://a", "name": "a", "_meta": { "k": 1 } }],
        "nextCursor": "c2",
    }))
    .unwrap();

    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].meta, Some(json!({ "k": 1 })));
    assert_eq!(next.as_deref(), Some("c2"));
}

#[test]
fn an_empty_cursor_ends_the_listing() {
    let (_, next) = parse_resource_page(&json!({ "resources": [], "nextCursor": "" })).unwrap();
    assert_eq!(next, None);
}

#[test]
fn a_page_without_resources_is_malformed() {
    let error = parse_resource_page(&json!({})).unwrap_err();
    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[test]
fn a_page_with_undecodable_entries_is_malformed() {
    let error = parse_resource_page(&json!({ "resources": [{ "name": 1 }] })).unwrap_err();
    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[test]
fn read_contents_are_kept_verbatim() {
    let contents = parse_read_result(
        "ui://a",
        &json!({ "contents": [
            { "uri": "ui://a", "mimeType": "text/html", "text": "<p>" },
            { "uri": "ui://a", "blob": "AAEC" },
        ] }),
    )
    .unwrap();

    assert_eq!(contents[0].text.as_deref(), Some("<p>"));
    assert_eq!(contents[1].blob.as_deref(), Some("AAEC"));
}

#[test]
fn read_contents_at_the_cap_are_accepted() {
    let text = "a".repeat(MAX_RESOURCE_BYTES);
    let contents = parse_read_result(
        "ui://a",
        &json!({ "contents": [{ "uri": "ui://a", "text": text }] }),
    )
    .unwrap();
    assert_eq!(contents[0].byte_len(), MAX_RESOURCE_BYTES);
}

#[test]
fn read_contents_above_the_cap_together_are_refused() {
    let half = "a".repeat(MAX_RESOURCE_BYTES / 2 + 1);
    let error = parse_read_result(
        "ui://a",
        &json!({ "contents": [
            { "uri": "ui://a", "text": half },
            { "uri": "ui://a", "blob": half },
        ] }),
    )
    .unwrap_err();

    assert!(matches!(error, Error::ResourceTooLarge { .. }), "{error:?}");
}

#[test]
fn a_read_reply_without_contents_is_malformed() {
    let error = parse_read_result("ui://a", &json!({})).unwrap_err();
    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
    let error = parse_read_result("ui://a", &json!({ "contents": 3 })).unwrap_err();
    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[test]
fn list_params_carry_the_cursor_only_when_there_is_one() {
    assert_eq!(list_params(None), json!({}));
    assert_eq!(list_params(Some("c")), json!({ "cursor": "c" }));
}
