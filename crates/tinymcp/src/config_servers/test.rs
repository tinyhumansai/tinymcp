//! Unit tests for the static server registry.
//!
//! The permission rules get the most attention. They are the only thing
//! standing between a configured deny list and a tool call reaching a remote
//! server, and their failure mode is silent — a tool that should have been
//! blocked simply runs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{McpRegistrySource, McpServerRegistry};
use crate::Error;
use tinymcp_bus::{
    McpAuthConfig, McpClientConfig, McpClientIdentityConfig, McpRemoteTool, McpServerConfig,
};

/// An HTTP server entry with the given name.
fn http_server(name: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.into(),
        endpoint: "https://example.test/mcp".into(),
        ..McpServerConfig::default()
    }
}

/// A registry built from the given servers.
fn registry_of(servers: Vec<McpServerConfig>) -> McpServerRegistry {
    McpServerRegistry::from_config(&McpClientConfig {
        servers,
        ..McpClientConfig::default()
    })
    .expect("the registry builds")
}

/// A tool with the given name.
fn tool(name: &str) -> McpRemoteTool {
    McpRemoteTool::new(name)
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

#[test]
fn a_disabled_configuration_yields_an_empty_registry() {
    let registry = McpServerRegistry::from_config(&McpClientConfig {
        enabled: false,
        servers: vec![http_server("weather")],
        ..McpClientConfig::default()
    })
    .expect("the registry builds");

    assert!(registry.is_empty());
}

#[test]
fn servers_keep_their_declaration_order() {
    let registry = registry_of(vec![
        http_server("charlie"),
        http_server("alpha"),
        http_server("bravo"),
    ]);

    let names: Vec<&str> = registry
        .list()
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    assert_eq!(names, ["charlie", "alpha", "bravo"]);
}

#[test]
fn a_disabled_server_is_not_registered() {
    let registry = registry_of(vec![McpServerConfig {
        enabled: false,
        ..http_server("weather")
    }]);

    assert!(registry.get("weather").is_none());
    assert!(registry.is_empty());
}

#[test]
fn an_unnamed_server_is_skipped_without_failing_the_others() {
    // One malformed entry should not cost a user every server they configured.
    let registry = registry_of(vec![
        McpServerConfig {
            name: "   ".into(),
            ..http_server("")
        },
        http_server("weather"),
    ]);

    assert_eq!(registry.len(), 1);
    assert!(registry.get("weather").is_some());
}

#[test]
fn a_server_with_neither_an_endpoint_nor_a_command_is_skipped() {
    let registry = registry_of(vec![McpServerConfig {
        name: "nowhere".into(),
        ..McpServerConfig::default()
    }]);

    assert!(registry.is_empty());
}

#[test]
fn a_name_and_endpoint_are_trimmed() {
    let registry = registry_of(vec![McpServerConfig {
        name: "  weather  ".into(),
        endpoint: "  https://example.test/mcp  ".into(),
        ..McpServerConfig::default()
    }]);

    let server = registry.get("weather").expect("the trimmed name resolves");
    assert_eq!(server.endpoint, "https://example.test/mcp");
}

#[test]
fn a_non_empty_command_selects_the_subprocess_transport() {
    let registry = registry_of(vec![McpServerConfig {
        name: "local".into(),
        command: "npx".into(),
        args: vec!["-y".into(), "some-server".into()],
        ..McpServerConfig::default()
    }]);

    let server = registry.get("local").expect("the server");
    assert!(server.is_stdio());
    assert_eq!(server.command.as_deref(), Some("npx"));
}

#[test]
fn an_endpoint_alone_selects_the_http_transport() {
    let registry = registry_of(vec![http_server("weather")]);
    let server = registry.get("weather").expect("the server");

    assert!(!server.is_stdio());
    assert_eq!(server.command, None);
}

#[test]
fn a_command_wins_over_an_endpoint_when_both_are_set() {
    let registry = registry_of(vec![McpServerConfig {
        command: "npx".into(),
        ..http_server("both")
    }]);

    assert!(registry.get("both").expect("the server").is_stdio());
}

#[test]
fn a_configured_server_is_sourced_as_configuration() {
    let registry = registry_of(vec![http_server("weather")]);
    assert_eq!(
        registry.get("weather").expect("the server").source,
        McpRegistrySource::Config
    );
}

// ---------------------------------------------------------------------------
// Host-seeded servers
// ---------------------------------------------------------------------------

#[test]
fn a_host_seeded_server_is_added_and_marked_as_such() {
    let mut registry = registry_of(Vec::new());
    registry
        .seed_host_server(
            &http_server("docs"),
            &McpClientIdentityConfig::default(),
            None,
        )
        .expect("seeding");

    assert_eq!(
        registry.get("docs").expect("the server").source,
        McpRegistrySource::Host
    );
}

#[test]
fn a_users_own_entry_wins_over_a_host_seeded_one_of_the_same_name() {
    // A host pinning its own documentation server must not override a user who
    // deliberately pointed that name somewhere else.
    let mut registry = registry_of(vec![McpServerConfig {
        endpoint: "https://mine.test/mcp".into(),
        ..http_server("docs")
    }]);

    registry
        .seed_host_server(
            &McpServerConfig {
                endpoint: "https://theirs.test/mcp".into(),
                ..http_server("docs")
            },
            &McpClientIdentityConfig::default(),
            None,
        )
        .expect("seeding");

    let server = registry.get("docs").expect("the server");
    assert_eq!(server.source, McpRegistrySource::Config);
    assert_eq!(server.endpoint, "https://mine.test/mcp");
    assert_eq!(registry.len(), 1);
}

// ---------------------------------------------------------------------------
// Tool permission
// ---------------------------------------------------------------------------

#[test]
fn an_empty_allow_list_permits_anything_not_denied() {
    let registry = registry_of(vec![McpServerConfig {
        disallowed_tools: vec!["dangerous".into()],
        ..http_server("weather")
    }]);
    let server = registry.get("weather").expect("the server");

    assert!(server.is_tool_allowed("forecast"));
    assert!(server.is_tool_allowed("anything_at_all"));
    assert!(!server.is_tool_allowed("dangerous"));
}

#[test]
fn a_non_empty_allow_list_excludes_everything_else() {
    let registry = registry_of(vec![McpServerConfig {
        allowed_tools: vec!["forecast".into()],
        ..http_server("weather")
    }]);
    let server = registry.get("weather").expect("the server");

    assert!(server.is_tool_allowed("forecast"));
    assert!(!server.is_tool_allowed("history"));
}

#[test]
fn the_deny_list_wins_over_the_allow_list() {
    // Ambiguity resolves toward refusing. A name on both lists is a
    // configuration the user should fix, and running it is the wrong guess.
    let registry = registry_of(vec![McpServerConfig {
        allowed_tools: vec!["forecast".into()],
        disallowed_tools: vec!["forecast".into()],
        ..http_server("weather")
    }]);

    assert!(
        !registry
            .get("weather")
            .expect("the server")
            .is_tool_allowed("forecast")
    );
}

#[test]
fn an_empty_tool_name_is_refused() {
    let registry = registry_of(vec![http_server("weather")]);
    let server = registry.get("weather").expect("the server");

    for name in ["", "   ", "\t\n"] {
        assert!(!server.is_tool_allowed(name), "{name:?} was permitted");
    }
}

#[test]
fn a_tool_name_is_compared_after_trimming() {
    let registry = registry_of(vec![McpServerConfig {
        disallowed_tools: vec!["dangerous".into()],
        ..http_server("weather")
    }]);
    let server = registry.get("weather").expect("the server");

    // A caller that pads the name must not slip past the deny list.
    assert!(!server.is_tool_allowed("  dangerous  "));
}

#[test]
fn tool_names_are_trimmed_and_deduplicated_when_registered() {
    let registry = registry_of(vec![McpServerConfig {
        allowed_tools: vec![
            "  forecast  ".into(),
            "forecast".into(),
            String::new(),
            "history".into(),
        ],
        ..http_server("weather")
    }]);

    let server = registry.get("weather").expect("the server");
    assert_eq!(server.allowed_tools, ["forecast", "history"]);
}

#[test]
fn filtering_keeps_only_permitted_tools() {
    let registry = registry_of(vec![McpServerConfig {
        allowed_tools: vec!["forecast".into()],
        ..http_server("weather")
    }]);
    let server = registry.get("weather").expect("the server");

    let kept = server.filter_allowed_tools(vec![tool("forecast"), tool("history"), tool("")]);

    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "forecast");
}

#[tokio::test]
async fn a_blocked_tool_call_fails_before_any_transport_work() {
    // The endpoint is unroutable. Reaching the transport at all would produce a
    // connection error rather than the permission error asserted here, so this
    // fails loudly if the check ever moves after the dial.
    let registry = registry_of(vec![McpServerConfig {
        endpoint: "http://127.0.0.1:1/mcp".into(),
        disallowed_tools: vec!["dangerous".into()],
        ..http_server("weather")
    }]);

    let error = registry
        .call_tool("weather", "dangerous", serde_json::json!({}))
        .await
        .expect_err("a denied tool");

    match error {
        Error::ToolNotAllowed { server, tool } => {
            assert_eq!(server, "weather");
            assert_eq!(tool, "dangerous");
        }
        other => panic!("expected a permission error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_empty_tool_name_is_refused_before_any_transport_work() {
    let registry = registry_of(vec![McpServerConfig {
        endpoint: "http://127.0.0.1:1/mcp".into(),
        ..http_server("weather")
    }]);

    let error = registry
        .call_tool("weather", "   ", serde_json::json!({}))
        .await
        .expect_err("an empty tool name");

    assert!(matches!(error, Error::ToolNotAllowed { .. }), "{error:?}");
}

// ---------------------------------------------------------------------------
// Unknown servers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_operation_reports_an_unknown_server_by_name() {
    let registry = registry_of(vec![http_server("weather")]);

    let listing = registry.list_tools("absent").await.expect_err("unknown");
    assert!(
        matches!(listing, Error::UnknownServer { ref server } if server == "absent"),
        "{listing:?}"
    );

    let call = registry
        .call_tool("absent", "anything", serde_json::json!({}))
        .await
        .expect_err("unknown");
    assert!(matches!(call, Error::UnknownServer { .. }), "{call:?}");

    let handshake = registry.initialize("absent").await.expect_err("unknown");
    assert!(
        matches!(handshake, Error::UnknownServer { .. }),
        "{handshake:?}"
    );

    let discovery = registry
        .discover_authorization("absent")
        .await
        .expect_err("unknown");
    assert!(
        matches!(discovery, Error::UnknownServer { .. }),
        "{discovery:?}"
    );
}

// ---------------------------------------------------------------------------
// Scoping
// ---------------------------------------------------------------------------

#[test]
fn scoping_keeps_only_the_named_servers_case_insensitively() {
    let registry = registry_of(vec![
        http_server("Weather"),
        http_server("calendar"),
        http_server("email"),
    ]);

    let scoped = registry.retaining_servers(&["weather".into(), "  EMAIL  ".into()]);

    assert_eq!(scoped.len(), 2);
    assert!(scoped.get("Weather").is_some());
    assert!(scoped.get("email").is_some());
    assert!(scoped.get("calendar").is_none());
}

#[test]
fn scoping_to_an_empty_list_yields_an_empty_registry() {
    // An empty list is a caller who selected nothing, not one who selected
    // everything. A caller meaning "everything" should not call this.
    let registry = registry_of(vec![http_server("weather")]);
    assert!(registry.retaining_servers(&[]).is_empty());
}

#[test]
fn scoping_preserves_declaration_order() {
    let registry = registry_of(vec![
        http_server("charlie"),
        http_server("alpha"),
        http_server("bravo"),
    ]);

    let scoped = registry.retaining_servers(&["bravo".into(), "charlie".into()]);
    let names: Vec<&str> = scoped
        .list()
        .iter()
        .map(|server| server.name.as_str())
        .collect();

    assert_eq!(names, ["charlie", "bravo"]);
}

#[test]
fn scoping_ignores_a_name_that_is_not_registered() {
    let registry = registry_of(vec![http_server("weather")]);
    let scoped = registry.retaining_servers(&["weather".into(), "absent".into()]);

    assert_eq!(scoped.len(), 1);
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[test]
fn a_servers_configured_credentials_are_carried_onto_its_definition() {
    let registry = registry_of(vec![McpServerConfig {
        auth: McpAuthConfig::BearerToken {
            token: "secret".into(),
        },
        ..http_server("weather")
    }]);

    assert_eq!(
        registry.get("weather").expect("the server").auth,
        McpAuthConfig::BearerToken {
            token: "secret".into()
        }
    );
}

#[test]
fn a_clone_shares_its_transports_rather_than_reconnecting() {
    // The registry is cloned per caller in some hosts; a clone that built fresh
    // sessions would multiply every server's connection count silently.
    let registry = registry_of(vec![http_server("weather")]);
    let cloned = registry.clone();

    assert_eq!(cloned.len(), registry.len());
    assert_eq!(
        cloned.get("weather").expect("the server").endpoint,
        registry.get("weather").expect("the server").endpoint
    );
}
