//! Unit tests for the shared transport surface.
//!
//! [`redact_endpoint`] gets the most attention here. It is the single control
//! standing between an MCP endpoint — which routinely carries an API key in a
//! query parameter — and every log line, error message, and telemetry event
//! this crate produces, so its failure modes are worth enumerating rather than
//! sampling.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::validate_protocol_version;
use crate::Error;
use crate::McpToolResultExt;
use tinymcp_bus::{LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS};

// ---------------------------------------------------------------------------
// validate_protocol_version
// ---------------------------------------------------------------------------

#[test]
fn every_supported_version_validates() {
    for version in SUPPORTED_PROTOCOL_VERSIONS {
        validate_protocol_version(version)
            .unwrap_or_else(|_| panic!("{version} is listed as supported but did not validate"));
    }
}

#[test]
fn the_latest_version_validates() {
    validate_protocol_version(LATEST_PROTOCOL_VERSION).expect("the latest version validates");
}

#[test]
fn an_unlisted_version_is_rejected_and_names_itself() {
    let error = validate_protocol_version("1999-01-01").expect_err("an unlisted version");

    match error {
        Error::UnsupportedProtocolVersion { version } => assert_eq!(version, "1999-01-01"),
        other => panic!("expected an unsupported-version error, got {other:?}"),
    }
}

#[test]
fn an_empty_version_is_rejected() {
    assert!(validate_protocol_version("").is_err());
}

#[test]
fn a_near_miss_version_is_rejected() {
    // Whitespace, a different separator, or a trailing character are all
    // rejections rather than near-enough matches.
    for version in [" 2025-11-25", "2025-11-25 ", "2025/11/25", "2025-11-250"] {
        assert!(
            validate_protocol_version(version).is_err(),
            "{version} was accepted"
        );
    }
}

#[test]
fn endpoint_redaction_drops_sensitive_url_parts() {
    assert_eq!(
        super::redact_endpoint("https://example.test/mcp?key=secret"),
        "https://example.test"
    );
    assert_eq!(
        super::redact_endpoint("https://user:pass@example.test"),
        "<redacted>"
    );
}

#[test]
fn tool_results_render_text_and_error_status() {
    let result = super::render_tool_result(&serde_json::json!({
        "isError": true,
        "content": [{"type": "text", "text": "invalid"}],
    }));
    assert!(result.is_error);
    assert_eq!(result.text(), "invalid");
}

#[test]
fn tool_result_without_text_falls_back_to_json() {
    let reply = serde_json::json!({ "isError": "not a boolean", "content": "not an array" });
    let rendered = super::render_tool_result(&reply);
    assert!(!rendered.is_error);
    assert_eq!(rendered.text(), reply.to_string());
}
