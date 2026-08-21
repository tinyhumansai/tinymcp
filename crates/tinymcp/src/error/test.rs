//! Unit tests for the crate-wide error type.
//!
//! Two properties matter beyond the obvious. Errors are *classified* by
//! variant, not by message text, so the predicates callers depend on are
//! exercised across every variant rather than only the one they answer `true`
//! for. And errors are printed — into logs, telemetry, and user interfaces — so
//! the rendering is checked for what it must never contain.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::Error;
use tinymcp_bus::McpAuthChallenge;

/// A 401 that advertised OAuth.
fn oauth_challenge_error() -> Error {
    Error::Unauthorized {
        endpoint: "https://example.test".into(),
        resource_metadata: Some("https://example.test/.well-known/oauth-protected-resource".into()),
    }
}

/// A 401 that advertised nothing.
fn bare_unauthorized_error() -> Error {
    Error::Unauthorized {
        endpoint: "https://example.test".into(),
        resource_metadata: None,
    }
}

/// One of every variant that does not need a live `reqwest` failure to build.
fn assorted_other_errors() -> Vec<Error> {
    vec![
        Error::Http {
            endpoint: "https://example.test".into(),
            status: 500,
            body: "boom".into(),
        },
        Error::UnsupportedProtocolVersion {
            version: "1999-01-01".into(),
        },
        Error::MalformedResponse {
            detail: "no result member".into(),
        },
        Error::Rpc {
            message: "method not found".into(),
        },
        Error::MissingAuthChallenge,
        Error::AuthDiscovery {
            detail: "unreachable".into(),
            challenge: Box::new(McpAuthChallenge {
                scheme: "Bearer".into(),
                realm: None,
                resource_metadata: None,
            }),
        },
        Error::ToolNotAllowed {
            server: "weather".into(),
            tool: "delete_everything".into(),
        },
        Error::UnknownServer {
            server: "nope".into(),
        },
    ]
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

#[test]
fn a_401_is_reported_as_unauthorized() {
    assert!(bare_unauthorized_error().is_unauthorized());
    assert!(oauth_challenge_error().is_unauthorized());
}

#[test]
fn no_other_variant_is_reported_as_unauthorized() {
    for error in assorted_other_errors() {
        assert!(
            !error.is_unauthorized(),
            "{error:?} was misclassified as unauthorized"
        );
    }
}

#[test]
fn only_a_401_advertising_resource_metadata_is_flagged_as_oauth() {
    // This is what decides between offering a sign-in and offering a token
    // field. A server that only accepts OAuth refuses a pasted token however
    // valid it looks.
    assert!(oauth_challenge_error().advertises_oauth());
    assert!(!bare_unauthorized_error().advertises_oauth());
}

#[test]
fn no_other_variant_is_flagged_as_oauth() {
    for error in assorted_other_errors() {
        assert!(
            !error.advertises_oauth(),
            "{error:?} was misclassified as advertising oauth"
        );
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[test]
fn every_message_is_lowercase_and_unpunctuated() {
    // The repository's convention, and worth a test because the messages are
    // written one at a time and read all together.
    let mut errors = assorted_other_errors();
    errors.push(bare_unauthorized_error());

    for error in errors {
        let rendered = error.to_string();
        let first = rendered.chars().next().expect("a message is never empty");
        assert!(
            !first.is_uppercase(),
            "{rendered:?} starts with a capital letter"
        );
        assert!(
            !rendered.ends_with('.'),
            "{rendered:?} ends with a full stop"
        );
    }
}

#[test]
fn an_unauthorized_error_names_its_endpoint() {
    assert!(
        bare_unauthorized_error()
            .to_string()
            .contains("https://example.test")
    );
}

#[test]
fn an_unauthorized_error_states_its_status() {
    // A host classifying errors for its own reporting may have only the
    // rendered text: the failure crosses an RPC boundary and comes back as a
    // string. Without the status it reads as an ordinary transport failure, and
    // preventable user state gets reported as an error once per retry.
    for error in [bare_unauthorized_error(), oauth_challenge_error()] {
        let rendered = error.to_string().to_lowercase();
        assert!(rendered.contains("mcp unauthorized for "), "{rendered}");
        assert!(rendered.contains("(http 401"), "{rendered}");
    }
}

#[test]
fn an_unauthorized_error_never_prints_the_oauth_metadata_url() {
    // The metadata URL is for the caller to act on, not to display: it is a
    // detail of the server's authorization setup and belongs in the affordance
    // the caller builds, not in a log line.
    let rendered = oauth_challenge_error().to_string();
    assert!(!rendered.contains(".well-known"), "{rendered}");
}

#[test]
fn a_blocked_tool_names_both_the_server_and_the_tool() {
    let rendered = Error::ToolNotAllowed {
        server: "weather".into(),
        tool: "delete_everything".into(),
    }
    .to_string();

    assert!(rendered.contains("weather"), "{rendered}");
    assert!(rendered.contains("delete_everything"), "{rendered}");
}

#[test]
fn an_http_error_says_what_the_server_answered() {
    // The body is where the server says *why*. A token endpoint answering
    // `invalid_grant` reads differently from one answering `invalid_client`,
    // and a caller that only sees the status cannot tell a user which.
    let rendered = Error::Http {
        endpoint: "https://example.test".into(),
        status: 400,
        body: "{\"error\":\"invalid_grant\"}".into(),
    }
    .to_string();

    assert!(rendered.contains("invalid_grant"), "{rendered}");
}

#[test]
fn an_http_error_with_no_body_does_not_trail_a_separator() {
    let rendered = Error::Http {
        endpoint: "https://example.test".into(),
        status: 502,
        body: "   ".into(),
    }
    .to_string();

    assert!(rendered.ends_with('`'), "{rendered}");
}

#[test]
fn a_very_long_failure_body_is_bounded_in_the_message() {
    // These reach logs, telemetry, and user-facing errors. An upstream that
    // answers a failure with a whole HTML page would otherwise put all of it
    // in every one.
    let rendered = Error::Http {
        endpoint: "https://example.test".into(),
        status: 500,
        body: "x".repeat(5_000),
    }
    .to_string();

    assert!(rendered.len() < 400, "{} bytes", rendered.len());
    assert!(rendered.ends_with('…'), "{rendered}");
}

#[test]
fn a_failure_body_is_bounded_on_a_character_boundary() {
    // Splitting a multi-byte character mid-sequence would panic.
    let rendered = Error::Http {
        endpoint: "https://example.test".into(),
        status: 500,
        body: "é".repeat(500),
    }
    .to_string();

    assert!(rendered.ends_with('…'), "{rendered}");
}

#[test]
fn an_http_error_names_its_status() {
    let rendered = Error::Http {
        endpoint: "https://example.test".into(),
        status: 503,
        body: String::new(),
    }
    .to_string();

    assert!(rendered.contains("503"), "{rendered}");
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

#[test]
fn a_serde_failure_converts_into_a_serialization_error() {
    let failure = serde_json::from_str::<serde_json::Value>("{ not json").expect_err("invalid");
    let error = Error::from(failure);

    assert!(matches!(error, Error::Serialization { .. }), "{error:?}");
}

#[test]
fn a_serialization_error_exposes_its_cause() {
    // The `#[source]` chain is what a logger walks, so it has to be attached.
    let failure = serde_json::from_str::<serde_json::Value>("{ not json").expect_err("invalid");
    let error = Error::from(failure);

    assert!(
        std::error::Error::source(&error).is_some(),
        "the serde failure was not attached as a cause"
    );
}

#[test]
fn the_malformed_helper_carries_its_detail_through() {
    let error = Error::malformed("the sky is falling");

    match error {
        Error::MalformedResponse { detail } => assert_eq!(detail, "the sky is falling"),
        other => panic!("expected a malformed-response error, got {other:?}"),
    }
}
