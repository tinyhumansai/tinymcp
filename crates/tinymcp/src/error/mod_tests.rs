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

/// A registry search that ran out of time.
fn registry_timeout() -> Error {
    Error::RegistryTimeout {
        endpoint: "https://registry.test/v0/servers".into(),
        operation: crate::registry::RegistryOperation::Search,
        timeout: std::time::Duration::from_secs(8),
    }
}

/// An HTTP failure with `status`.
fn http(status: u16) -> Error {
    Error::Http {
        endpoint: "https://registry.test".into(),
        status,
        body: String::new(),
    }
}

/// One of every variant that does not need a live `reqwest` failure to build.
fn assorted_other_errors() -> Vec<Error> {
    vec![
        registry_timeout(),
        Error::Http {
            endpoint: "https://example.test".into(),
            status: 500,
            body: "boom".into(),
        },
        Error::UnsupportedProtocolVersion {
            version: "1999-01-01".into(),
        },
        Error::CredentialStore {
            action: "writing credentials".into(),
            detail: "the vault is sealed".into(),
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
        Error::ConfigDoc {
            detail: "`a` needs a `url`".into(),
        },
        Error::InvalidArguments {
            tool: "forecast".into(),
            reason: tinymcp_bus::ArgsError::NotAnObject { actual: "a number" },
        },
        Error::ServerIo {
            source: Box::new(std::io::Error::other("pipe closed")),
        },
        Error::ServerBind {
            addr: "127.0.0.1:9300".parse().unwrap(),
            source: Box::new(std::io::Error::other("address in use")),
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
fn only_a_401_with_discovered_oauth_metadata_is_flagged_as_oauth() {
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

// ---------------------------------------------------------------------------
// The names an error travels under
// ---------------------------------------------------------------------------

/// A `reqwest` failure that needs no network: a URL that cannot be parsed.
fn a_reqwest_error() -> reqwest::Error {
    reqwest::Client::new()
        .get("not a url")
        .build()
        .expect_err("that is not a url")
}

/// One of every variant, including the ones that wrap a foreign error.
fn one_of_every_variant() -> Vec<Error> {
    let mut errors = assorted_other_errors();
    errors.push(oauth_challenge_error());
    errors.push(Error::MissingRuntime {
        command: "uvx".into(),
        runtime: tinymcp_bus::CommandKind::Python,
    });
    errors.push(Error::Transport {
        endpoint: "https://example.test".into(),
        source: Box::new(a_reqwest_error()),
    });
    errors.push(Error::NotConnected {
        server: "srv".into(),
    });
    errors.push(Error::ServerDisabled {
        server: "srv".into(),
    });
    errors.push(Error::ResourceTooLarge {
        uri: "ui://card".into(),
        bytes: 3,
        limit: 2,
    });
    errors.push(Error::ClientBuild {
        source: Box::new(a_reqwest_error()),
    });
    errors.push(serde_json::from_str::<u8>("x").unwrap_err().into());
    errors.push(Error::store(
        "listing",
        rusqlite::Error::QueryReturnedNoRows,
    ));
    errors.push(Error::StoreIo {
        path: "/nowhere".into(),
        source: Box::new(std::io::Error::other("denied")),
    });
    errors.push(Error::Bus {
        detail: "refused".into(),
    });
    errors.push(Error::invalid_argument("not absolute"));
    errors
}

#[test]
fn every_variant_travels_under_a_name_in_the_contract_table() {
    for error in one_of_every_variant() {
        assert!(
            tinymcp_bus::errors::ALL.contains(&error.wire_name()),
            "{error} maps to {}, which the contract does not list",
            error.wire_name()
        );
    }
}

#[test]
fn every_name_in_the_contract_table_is_produced_by_some_variant() {
    // The other direction, so a name cannot sit in the table with nothing
    // behind it — a host would be matching on something that never arrives.
    let produced: std::collections::BTreeSet<&str> = one_of_every_variant()
        .iter()
        .map(Error::wire_name)
        .collect();
    let listed: std::collections::BTreeSet<&str> =
        tinymcp_bus::errors::ALL.iter().copied().collect();

    assert_eq!(produced, listed);
}

#[test]
fn no_two_variants_share_a_name() {
    let errors = one_of_every_variant();
    let names: std::collections::BTreeSet<&str> = errors.iter().map(Error::wire_name).collect();

    assert_eq!(names.len(), errors.len());
}

#[test]
fn an_unauthorized_error_carries_the_name_a_host_classifies_on() {
    assert_eq!(
        oauth_challenge_error().wire_name(),
        tinymcp_bus::errors::UNAUTHORIZED
    );
    assert_eq!(
        bare_unauthorized_error().wire_name(),
        tinymcp_bus::errors::UNAUTHORIZED
    );
}

#[test]
fn an_invalid_argument_error_says_which_rule_without_a_value() {
    let error = Error::invalid_argument("a data directory must be absolute");

    assert_eq!(
        error.to_string(),
        "invalid argument: a data directory must be absolute"
    );
}

#[test]
fn a_bus_error_names_what_the_bus_reported() {
    let error = Error::Bus {
        detail: "refused".into(),
    };

    assert_eq!(error.to_string(), "bus failure: refused");
}

#[test]
fn server_failures_name_what_failed_and_keep_their_cause() {
    use std::error::Error as _;

    let io = Error::ServerIo {
        source: Box::new(std::io::Error::other("pipe closed")),
    };
    assert_eq!(io.to_string(), "mcp server i/o failure: pipe closed");
    assert_eq!(io.source().unwrap().to_string(), "pipe closed");

    let bind = Error::ServerBind {
        addr: "127.0.0.1:9300".parse().unwrap(),
        source: Box::new(std::io::Error::other("address in use")),
    };
    assert_eq!(
        bind.to_string(),
        "could not bind the mcp server on `127.0.0.1:9300`: address in use"
    );
    assert_eq!(bind.source().unwrap().to_string(), "address in use");
}

// ---------------------------------------------------------------------------
// Registry outages
// ---------------------------------------------------------------------------

#[test]
fn a_registry_timeout_names_the_operation_and_its_budget() {
    let rendered = registry_timeout().to_string();

    assert!(rendered.contains("search"), "{rendered}");
    assert!(rendered.contains("8000ms"), "{rendered}");
    assert!(
        rendered.contains("https://registry.test/v0/servers"),
        "{rendered}"
    );
}

#[test]
fn a_registry_timeout_travels_under_its_own_name() {
    assert_eq!(
        registry_timeout().wire_name(),
        tinymcp_bus::errors::REGISTRY_TIMEOUT
    );
}

#[test]
fn a_registry_timeout_is_a_timeout_and_an_outage() {
    let error = registry_timeout();

    assert!(error.is_timeout());
    assert!(error.is_registry_unavailable());
    assert!(!error.is_unauthorized());
}

#[test]
fn a_transport_failure_is_an_outage_but_not_necessarily_a_timeout() {
    let error = Error::Transport {
        endpoint: "https://registry.test".into(),
        source: Box::new(a_reqwest_error()),
    };

    assert!(error.is_registry_unavailable());
    assert!(!error.is_timeout());
}

#[test]
fn statuses_naming_the_upstream_are_outages() {
    for status in [408, 429, 500, 502, 503, 504, 599] {
        assert!(http(status).is_registry_unavailable(), "{status}");
    }
}

#[test]
fn statuses_naming_the_request_are_not_outages() {
    for status in [400, 401, 403, 404, 422, 600] {
        assert!(!http(status).is_registry_unavailable(), "{status}");
    }
    assert!(!http(503).is_timeout());
}

#[test]
fn only_outage_variants_count_as_registry_unavailable() {
    for error in assorted_other_errors() {
        let expected = matches!(error, Error::RegistryTimeout { .. } | Error::Http { .. });
        assert_eq!(error.is_registry_unavailable(), expected, "{error:?}");
    }
    assert!(!oauth_challenge_error().is_registry_unavailable());
    assert!(!bare_unauthorized_error().is_timeout());
}
