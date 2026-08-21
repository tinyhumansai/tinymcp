//! Unit tests for the registry payload types.
//!
//! Three things are pinned here, in descending order of how badly getting them
//! wrong would hurt:
//!
//! 1. **The persistence formats.** `Transport::dispatch_kind` and
//!    `CommandKind::as_str` become column values. A silent rename orphans every
//!    stored row.
//! 2. **The trust signals.** `website_url` and `auth_kind` decide whether a
//!    server passes curation, and `skip_deserializing` is the only thing
//!    stopping an upstream from setting them itself.
//! 3. **The wire form**, in both directions: camelCase in from the registries,
//!    snake_case out to a host's own consumers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{
    ChatTurn, CommandKind, ConnStatus, ConnectedServerOverview, InstalledServer, McpAuthHint,
    McpTool, RegistryConnection, RegistryListResponse, RegistryServerDetail,
    RegistryServerSummary, ServerStatus, Transport,
};
use serde_json::json;

/// A fully populated stdio install, for tests that need one.
fn an_installed_server() -> InstalledServer {
    InstalledServer {
        server_id: "uuid-1".into(),
        qualified_name: "@test/server".into(),
        display_name: "Test".into(),
        description: None,
        icon_url: None,
        command_kind: CommandKind::Node,
        command: "npx".into(),
        args: vec!["-y".into(), "@test/server".into()],
        env_keys: vec!["API_KEY".into()],
        config: None,
        installed_at: 1_700_000_000_000,
        last_connected_at: None,
        transport: Transport::Stdio,
        enabled: true,
    }
}

// ---------------------------------------------------------------------------
// CommandKind
// ---------------------------------------------------------------------------

#[test]
fn command_kind_round_trips_through_its_persisted_string() {
    for kind in [CommandKind::Node, CommandKind::Python, CommandKind::Binary] {
        assert_eq!(CommandKind::parse(kind.as_str()), kind);
    }
}

#[test]
fn command_kind_strings_are_pinned() {
    assert_eq!(CommandKind::Node.as_str(), "node");
    assert_eq!(CommandKind::Python.as_str(), "python");
    assert_eq!(CommandKind::Binary.as_str(), "binary");
}

#[test]
fn an_unrecognised_command_kind_becomes_node() {
    // `npx` is what the overwhelming majority of listings use, so an
    // unrecognised value is far likelier to be a stale row than a new
    // ecosystem.
    assert_eq!(CommandKind::parse(""), CommandKind::Node);
    assert_eq!(CommandKind::parse("nonsense"), CommandKind::Node);
    assert_eq!(CommandKind::parse("NODE"), CommandKind::Node);
}

#[test]
fn command_kind_serializes_lowercase() {
    assert_eq!(
        serde_json::to_value(CommandKind::Python).unwrap(),
        json!("python")
    );
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

#[test]
fn transport_dispatch_kinds_are_pinned() {
    // These are column values in the install row. A rename here orphans every
    // stored server.
    assert_eq!(Transport::Stdio.dispatch_kind(), "stdio");
    assert_eq!(
        Transport::HttpRemote {
            url: "https://example.test/mcp".into()
        }
        .dispatch_kind(),
        "http_remote"
    );
}

#[test]
fn transport_parse_falls_back_to_stdio_for_anything_unrecognised() {
    assert_eq!(Transport::parse("stdio", None), Transport::Stdio);
    assert_eq!(Transport::parse("stdio", Some("ignored")), Transport::Stdio);
    // A row written before the column existed.
    assert_eq!(Transport::parse("", None), Transport::Stdio);
    // A row from some future version. Better to stall on connect than to
    // misroute to a transport this build was never meant to use.
    assert_eq!(Transport::parse("garbage", None), Transport::Stdio);
}

#[test]
fn transport_parse_carries_the_url_through_for_http_remote() {
    assert_eq!(
        Transport::parse("http_remote", Some("https://x.test/mcp")),
        Transport::HttpRemote {
            url: "https://x.test/mcp".into()
        }
    );
}

#[test]
fn an_http_remote_transport_with_no_url_parses_to_an_empty_one() {
    // A malformed row should still load; the empty endpoint fails at dial time
    // with a clear error rather than at parse time with a lost record.
    assert_eq!(
        Transport::parse("http_remote", None),
        Transport::HttpRemote { url: String::new() }
    );
}

#[test]
fn the_deployment_url_accessor_never_crosses_the_two_transports() {
    assert_eq!(Transport::Stdio.deployment_url(), None);
    assert_eq!(
        Transport::HttpRemote {
            url: "https://smithery.test/server/x".into()
        }
        .deployment_url(),
        Some("https://smithery.test/server/x")
    );
}

#[test]
fn transport_round_trips_through_its_tagged_json() {
    for transport in [
        Transport::Stdio,
        Transport::HttpRemote {
            url: "https://x.test/mcp".into(),
        },
    ] {
        let encoded = serde_json::to_value(&transport).unwrap();
        assert_eq!(encoded["kind"], json!(transport.dispatch_kind()));
        assert_eq!(
            serde_json::from_value::<Transport>(encoded).unwrap(),
            transport
        );
    }
}

// ---------------------------------------------------------------------------
// InstalledServer
// ---------------------------------------------------------------------------

#[test]
fn an_install_serializes_env_key_names_and_no_values() {
    let encoded = serde_json::to_value(an_installed_server()).unwrap();
    assert_eq!(encoded["env_keys"], json!(["API_KEY"]));
    assert!(
        encoded.get("env_values").is_none(),
        "an install record must never carry credential values"
    );
    assert!(encoded.get("env").is_none());
}

#[test]
fn an_install_from_before_the_transport_and_enabled_fields_still_loads() {
    // Rows persisted before those columns existed were all enabled stdio
    // installs. Without the defaults, every one of them would fail to load
    // after an upgrade.
    let legacy = json!({
        "server_id": "uuid-1",
        "qualified_name": "@old/server",
        "display_name": "Old",
        "description": null,
        "icon_url": null,
        "command_kind": "node",
        "command": "npx",
        "args": ["-y", "@old/server"],
        "env_keys": [],
        "config": null,
        "installed_at": 1_700_000_000_000i64,
        "last_connected_at": null,
    });

    let server: InstalledServer = serde_json::from_value(legacy).unwrap();
    assert_eq!(server.transport, Transport::Stdio);
    assert!(server.enabled);
}

#[test]
fn an_install_round_trips() {
    let server = InstalledServer {
        transport: Transport::HttpRemote {
            url: "https://x.test/mcp".into(),
        },
        config: Some(json!({ "region": "eu" })),
        last_connected_at: Some(1_700_000_001_000),
        enabled: false,
        ..an_installed_server()
    };
    let encoded = serde_json::to_value(&server).unwrap();
    assert_eq!(
        serde_json::from_value::<InstalledServer>(encoded).unwrap(),
        server
    );
}

// ---------------------------------------------------------------------------
// McpTool and ConnectedServerOverview
// ---------------------------------------------------------------------------

#[test]
fn a_tool_needs_only_its_name() {
    let tool: McpTool = serde_json::from_value(json!({ "name": "ping" })).unwrap();
    assert_eq!(tool, McpTool::new("ping"));
}

#[test]
fn an_overview_round_trips_with_its_tools() {
    let overview = ConnectedServerOverview {
        server_id: "uuid-1".into(),
        qualified_name: "@test/server".into(),
        display_name: "Test".into(),
        description: Some("does things".into()),
        tools: vec![McpTool::new("a"), McpTool::new("b")],
    };
    let encoded = serde_json::to_value(&overview).unwrap();
    assert_eq!(
        serde_json::from_value::<ConnectedServerOverview>(encoded).unwrap(),
        overview
    );
}

// ---------------------------------------------------------------------------
// ServerStatus and McpAuthHint
// ---------------------------------------------------------------------------

#[test]
fn server_status_strings_are_pinned() {
    assert_eq!(ServerStatus::Disconnected.as_str(), "disconnected");
    assert_eq!(ServerStatus::Connecting.as_str(), "connecting");
    assert_eq!(ServerStatus::Connected.as_str(), "connected");
    assert_eq!(ServerStatus::Unauthorized.as_str(), "unauthorized");
    assert_eq!(ServerStatus::Error.as_str(), "error");
    assert_eq!(ServerStatus::Disabled.as_str(), "disabled");
}

#[test]
fn every_server_status_serializes_to_its_own_string() {
    for status in [
        ServerStatus::Disconnected,
        ServerStatus::Connecting,
        ServerStatus::Connected,
        ServerStatus::Unauthorized,
        ServerStatus::Error,
        ServerStatus::Disabled,
    ] {
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!(status.as_str()),
            "{status:?} serializes to something other than its own as_str"
        );
    }
}

#[test]
fn auth_hint_codes_are_pinned() {
    // These drive which re-authentication affordance a user is offered.
    assert_eq!(McpAuthHint::OauthRequired.as_code(), "oauth_required");
    assert_eq!(McpAuthHint::TokenRejected.as_code(), "token_rejected");
    assert_eq!(
        McpAuthHint::CredentialRequired.as_code(),
        "credential_required"
    );
}

#[test]
fn every_auth_hint_serializes_to_its_own_code() {
    for hint in [
        McpAuthHint::OauthRequired,
        McpAuthHint::TokenRejected,
        McpAuthHint::CredentialRequired,
    ] {
        assert_eq!(serde_json::to_value(hint).unwrap(), json!(hint.as_code()));
        assert_eq!(
            serde_json::from_value::<McpAuthHint>(json!(hint.as_code())).unwrap(),
            hint
        );
    }
}

#[test]
fn an_advertised_oauth_challenge_outranks_a_supplied_credential() {
    // The case this exists for: a server that only accepts OAuth answers 401 to
    // a pasted bearer token. Reporting "token rejected" would send the user to
    // fix a credential that was never going to work.
    assert_eq!(
        McpAuthHint::classify(true, true),
        McpAuthHint::OauthRequired
    );
    assert_eq!(
        McpAuthHint::classify(true, false),
        McpAuthHint::OauthRequired
    );
}

#[test]
fn a_refused_credential_is_reported_as_a_rejected_token() {
    assert_eq!(
        McpAuthHint::classify(false, true),
        McpAuthHint::TokenRejected
    );
}

#[test]
fn a_missing_credential_is_reported_as_one_being_required() {
    assert_eq!(
        McpAuthHint::classify(false, false),
        McpAuthHint::CredentialRequired
    );
}

// ---------------------------------------------------------------------------
// ConnStatus
// ---------------------------------------------------------------------------

#[test]
fn a_status_serializes_its_state_lowercase_and_omits_an_absent_hint() {
    let status = ConnStatus {
        server_id: "s1".into(),
        qualified_name: "@test/s".into(),
        display_name: "S".into(),
        status: ServerStatus::Connected,
        tool_count: 3,
        last_error: None,
        auth_hint: None,
    };
    let encoded = serde_json::to_value(&status).unwrap();
    assert_eq!(encoded["status"], json!("connected"));
    assert!(
        encoded.get("auth_hint").is_none(),
        "an absent hint stays off the wire entirely"
    );
}

#[test]
fn a_status_carrying_a_hint_serializes_it_as_its_code() {
    let status = ConnStatus {
        server_id: "s1".into(),
        qualified_name: "@test/s".into(),
        display_name: "S".into(),
        status: ServerStatus::Unauthorized,
        tool_count: 0,
        last_error: Some("HTTP 401".into()),
        auth_hint: Some(McpAuthHint::OauthRequired),
    };
    let encoded = serde_json::to_value(&status).unwrap();
    assert_eq!(encoded["status"], json!("unauthorized"));
    assert_eq!(encoded["auth_hint"], json!("oauth_required"));
    assert_eq!(
        serde_json::from_value::<ConnStatus>(encoded).unwrap(),
        status
    );
}

// ---------------------------------------------------------------------------
// Registry DTOs — the trust signals
// ---------------------------------------------------------------------------

#[test]
fn a_summary_never_takes_its_trust_signals_from_the_wire() {
    // `website_url` and `auth_kind` decide whether a server passes the strict
    // catalog filter. `skip_deserializing` is the entire control; without it an
    // upstream could badge itself into curation by emitting two keys.
    let summary: RegistryServerSummary = serde_json::from_value(json!({
        "qualifiedName": "@evil/server",
        "displayName": "Evil",
        "website_url": "https://spoofed.example",
        "auth_kind": "api_key",
    }))
    .unwrap();

    assert_eq!(summary.website_url, None);
    assert_eq!(summary.auth_kind, None);
}

#[test]
fn a_summary_never_takes_its_trust_signals_from_their_camel_case_spellings_either() {
    let summary: RegistryServerSummary = serde_json::from_value(json!({
        "qualifiedName": "@evil/server",
        "displayName": "Evil",
        "websiteUrl": "https://spoofed.example",
        "authKind": "api_key",
    }))
    .unwrap();

    assert_eq!(summary.website_url, None);
    assert_eq!(summary.auth_kind, None);
    // An unmodelled key is preserved rather than dropped, but it lands in
    // `extra`, where nothing consults it for curation.
    assert!(summary.extra.contains_key("websiteUrl"));
}

#[test]
fn a_summary_still_serializes_the_trust_signals_the_adapter_set() {
    let mut summary: RegistryServerSummary = serde_json::from_value(json!({
        "qualified_name": "@test/s",
        "display_name": "S",
    }))
    .unwrap();
    summary.website_url = Some("https://vendor.test".into());
    summary.auth_kind = Some("api_key".into());

    let encoded = serde_json::to_value(&summary).unwrap();
    assert_eq!(encoded["website_url"], json!("https://vendor.test"));
    assert_eq!(encoded["auth_kind"], json!("api_key"));
}

// ---------------------------------------------------------------------------
// Registry DTOs — the two spellings
// ---------------------------------------------------------------------------

#[test]
fn a_summary_decodes_from_smitherys_camel_case() {
    let summary: RegistryServerSummary = serde_json::from_value(json!({
        "qualifiedName": "@test/server",
        "displayName": "Test Server",
        "iconUrl": "https://example.test/i.png",
        "useCount": 42,
        "isDeployed": true,
    }))
    .unwrap();

    assert_eq!(summary.qualified_name, "@test/server");
    assert_eq!(summary.display_name, "Test Server");
    assert_eq!(summary.icon_url.as_deref(), Some("https://example.test/i.png"));
    assert_eq!(summary.use_count, 42);
    assert!(summary.is_deployed);
}

#[test]
fn a_summary_decodes_from_the_official_adapters_snake_case() {
    let summary: RegistryServerSummary = serde_json::from_value(json!({
        "qualified_name": "@test/snake",
        "display_name": "Snake Test",
        "icon_url": "https://example.test/i.png",
        "use_count": 42,
        "is_deployed": true,
    }))
    .unwrap();

    assert_eq!(summary.qualified_name, "@test/snake");
    assert_eq!(summary.display_name, "Snake Test");
    assert_eq!(summary.use_count, 42);
}

#[test]
fn a_summary_tolerates_everything_optional_being_absent() {
    let summary: RegistryServerSummary = serde_json::from_value(json!({
        "qualifiedName": "@test/server",
        "displayName": "Test Server",
    }))
    .unwrap();

    assert!(summary.description.is_none());
    assert_eq!(summary.use_count, 0);
    assert!(!summary.is_deployed);
    assert!(!summary.official);
    assert!(summary.source.is_empty());
}

#[test]
fn a_summary_always_serializes_snake_case() {
    let summary: RegistryServerSummary = serde_json::from_value(json!({
        "qualifiedName": "@test/ser",
        "displayName": "Ser Test",
        "iconUrl": "https://example.test/i.png",
        "useCount": 10,
        "isDeployed": true,
    }))
    .unwrap();

    let encoded = serde_json::to_value(&summary).unwrap();
    for key in [
        "qualified_name",
        "display_name",
        "icon_url",
        "use_count",
        "is_deployed",
    ] {
        assert!(encoded.get(key).is_some(), "missing {key}");
    }
    for key in ["qualifiedName", "displayName", "iconUrl", "useCount", "isDeployed"] {
        assert!(encoded.get(key).is_none(), "leaked camelCase {key}");
    }
}

#[test]
fn a_detail_always_serializes_snake_case() {
    let detail: RegistryServerDetail = serde_json::from_value(json!({
        "qualifiedName": "@test/d",
        "displayName": "Detail",
    }))
    .unwrap();

    let encoded = serde_json::to_value(&detail).unwrap();
    assert!(encoded.get("qualified_name").is_some());
    assert!(encoded.get("display_name").is_some());
    assert!(encoded.get("qualifiedName").is_none());
}

#[test]
fn a_connection_decodes_from_camel_case_and_serializes_snake_case() {
    let connection: RegistryConnection = serde_json::from_value(json!({
        "type": "stdio",
        "deploymentUrl": "https://x.test",
        "configSchema": { "properties": {} },
        "exampleConfig": { "command": "npx" },
        "published": true,
    }))
    .unwrap();

    assert_eq!(connection.r#type, "stdio");
    assert_eq!(connection.deployment_url.as_deref(), Some("https://x.test"));
    assert!(connection.config_schema.is_some());
    assert!(connection.example_config.is_some());

    let encoded = serde_json::to_value(&connection).unwrap();
    for key in ["deployment_url", "config_schema", "example_config"] {
        assert!(encoded.get(key).is_some(), "missing {key}");
    }
    assert!(encoded.get("deploymentUrl").is_none());
}

#[test]
fn a_list_response_parses_its_pagination_from_camel_case() {
    let response: RegistryListResponse = serde_json::from_value(json!({
        "servers": [],
        "pagination": {
            "currentPage": 1,
            "pageSize": 20,
            "totalPages": 3,
            "totalCount": 55,
        },
    }))
    .unwrap();

    assert_eq!(response.pagination.current_page, 1);
    assert_eq!(response.pagination.page_size, 20);
    assert_eq!(response.pagination.total_pages, 3);
    assert_eq!(response.pagination.total_count, 55);
}

#[test]
fn a_list_response_with_no_pagination_reads_as_zeroes() {
    let response: RegistryListResponse =
        serde_json::from_value(json!({ "servers": [] })).unwrap();
    assert_eq!(response.pagination.total_count, 0);
}

#[test]
fn unmodelled_upstream_fields_survive_a_round_trip() {
    // Preserving them is what lets an adapter pass through something this
    // contract has not learned about yet without a version bump.
    let summary: RegistryServerSummary = serde_json::from_value(json!({
        "qualifiedName": "@test/s",
        "displayName": "S",
        "somethingNew": { "nested": true },
    }))
    .unwrap();

    assert_eq!(summary.extra["somethingNew"], json!({ "nested": true }));
    let encoded = serde_json::to_value(&summary).unwrap();
    assert_eq!(encoded["somethingNew"], json!({ "nested": true }));
}

// ---------------------------------------------------------------------------
// ChatTurn
// ---------------------------------------------------------------------------

#[test]
fn a_chat_turn_round_trips() {
    let turn = ChatTurn::new("user", "how do I configure this?");
    let encoded = serde_json::to_value(&turn).unwrap();
    assert_eq!(
        encoded,
        json!({ "role": "user", "content": "how do I configure this?" })
    );
    assert_eq!(serde_json::from_value::<ChatTurn>(encoded).unwrap(), turn);
}
