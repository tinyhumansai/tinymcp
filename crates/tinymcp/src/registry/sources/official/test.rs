//! Unit tests for the official registry adapter.
//!
//! Most of this is shape conversion, and the shapes come from a registry whose
//! schema moves. The tests are built from JSON fixtures rather than from
//! constructed Rust values for exactly that reason: a fixture that no longer
//! parses is the failure worth catching, and a constructed value can never
//! reproduce it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};

use super::types::{OfficialListResponse, OfficialServer};
use super::{page_bound, search_cache_key};

/// A list response wrapping `servers`.
fn list_response(servers: &Value, next_cursor: Option<&str>) -> OfficialListResponse {
    let mut document = json!({ "servers": servers });
    if let Some(cursor) = next_cursor {
        document["metadata"] = json!({ "nextCursor": cursor });
    }
    serde_json::from_value(document).expect("a list response decodes")
}

/// One envelope around a server that can be installed.
fn envelope(name: &str) -> Value {
    json!({
        "server": {
            "name": name,
            "description": "a server",
            "packages": [{ "registryType": "npm", "identifier": name }],
        },
    })
}

// ---------------------------------------------------------------------------
// The envelope
// ---------------------------------------------------------------------------

#[test]
fn a_row_is_read_from_inside_its_envelope() {
    // The bug this guards: an earlier adapter parsed the inner shape at the top
    // level, so serde filled every field with a default and the catalog
    // rendered pages of blank cards.
    let response = list_response(&json!([envelope("io.github.example/server")]), None);
    let summaries = response.into_summaries();

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].qualified_name, "io.github.example/server");
    assert_eq!(summaries[0].description.as_deref(), Some("a server"));
}

#[test]
fn a_row_with_no_server_key_is_a_parse_error_rather_than_a_blank_card() {
    // Everything else here is permissive so a schema bump does not break the
    // catalog. This one must be loud, or the blank-card failure returns looking
    // like an empty registry.
    let decoded = serde_json::from_value::<OfficialListResponse>(json!({
        "servers": [{ "name": "flat-shape", "description": "wrong level" }],
    }));

    assert!(decoded.is_err(), "a flat row should not decode");
}

#[test]
fn an_empty_response_yields_no_rows() {
    assert!(list_response(&json!([]), None).into_summaries().is_empty());
    let empty: OfficialListResponse = serde_json::from_value(json!({})).unwrap();
    assert!(empty.into_summaries().is_empty());
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

#[test]
fn a_row_offering_no_way_to_connect_is_dropped() {
    // A user can only discover such a row is a dead end by trying to install
    // it.
    let response = list_response(
        &json!([{ "server": { "name": "nothing/here", "description": "no way in" } }]),
        None,
    );

    assert!(response.into_summaries().is_empty());
}

#[test]
fn a_row_with_only_a_remote_is_kept() {
    let response = list_response(
        &json!([{
            "server": {
                "name": "remote/only",
                "remotes": [{ "url": "https://api.test/mcp" }],
            },
        }]),
        None,
    );

    let summaries = response.into_summaries();
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].is_deployed, "a hosted remote is deployed");
}

#[test]
fn a_deprecated_row_is_dropped() {
    let response = list_response(
        &json!([{
            "server": {
                "name": "old/server",
                "packages": [{ "registryType": "npm", "identifier": "old" }],
            },
            "_meta": {
                "io.modelcontextprotocol.registry/official": { "status": "deprecated" },
            },
        }]),
        None,
    );

    assert!(response.into_summaries().is_empty());
}

#[test]
fn a_row_with_no_metadata_is_not_treated_as_deprecated() {
    // That is what a row cached by an older build looks like.
    let response = list_response(&json!([envelope("io.github.example/server")]), None);
    assert_eq!(response.into_summaries().len(), 1);
}

#[test]
fn a_row_with_an_active_status_is_kept() {
    let response = list_response(
        &json!([{
            "server": {
                "name": "live/server",
                "packages": [{ "registryType": "npm", "identifier": "live" }],
            },
            "_meta": {
                "io.modelcontextprotocol.registry/official": { "status": "active" },
            },
        }]),
        None,
    );

    assert_eq!(response.into_summaries().len(), 1);
}

#[test]
fn a_repeated_name_appears_once() {
    let response = list_response(&json!([envelope("same/name"), envelope("same/name")]), None);

    assert_eq!(response.into_summaries().len(), 1);
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

#[test]
fn a_declared_title_is_used_as_the_display_name() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "io.github.example/server-bar",
        "title": "Example Server",
    }))
    .unwrap();

    assert_eq!(server.display_name(), "Example Server");
}

#[test]
fn a_blank_title_falls_back_to_the_derived_name() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "io.github.example/server-bar",
        "title": "   ",
    }))
    .unwrap();

    assert_eq!(server.display_name(), "server bar");
}

#[test]
fn a_name_is_derived_from_its_last_segment_with_separators_spaced() {
    // Better to show than `io.github.someone/some-server`.
    for (name, expected) in [
        ("io.github.example/server-bar", "server bar"),
        ("io.github.example/server_bar", "server bar"),
        ("com.vendor.product", "product"),
        ("bare", "bare"),
    ] {
        let server: OfficialServer = serde_json::from_value(json!({ "name": name })).unwrap();
        assert_eq!(server.display_name(), expected, "for {name}");
    }
}

// ---------------------------------------------------------------------------
// Trust signals
// ---------------------------------------------------------------------------

#[test]
fn a_declared_website_becomes_the_trust_signal() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "websiteUrl": "https://vendor.test",
    }))
    .unwrap();

    assert_eq!(
        server.into_summary().website_url.as_deref(),
        Some("https://vendor.test")
    );
}

#[test]
fn a_blank_website_is_not_a_trust_signal() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "websiteUrl": "   ",
    }))
    .unwrap();

    assert_eq!(server.into_summary().website_url, None);
}

#[test]
fn a_secret_header_declares_a_static_credential() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{
            "url": "https://api.test/mcp",
            "headers": [{ "name": "X-Api-Key", "isSecret": true }],
        }],
    }))
    .unwrap();

    assert_eq!(server.into_summary().auth_kind.as_deref(), Some("api_key"));
}

#[test]
fn an_authorization_header_declares_a_static_credential_even_unmarked() {
    // Registries are inconsistent about marking it, and an `Authorization`
    // header is a credential whatever else it says.
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{
            "url": "https://api.test/mcp",
            "headers": [{ "name": "authorization" }],
        }],
    }))
    .unwrap();

    assert_eq!(server.into_summary().auth_kind.as_deref(), Some("api_key"));
}

#[test]
fn a_secret_environment_variable_declares_a_static_credential() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "npm",
            "identifier": "server",
            "environmentVariables": [{ "name": "API_KEY", "isSecret": true }],
        }],
    }))
    .unwrap();

    assert_eq!(server.into_summary().auth_kind.as_deref(), Some("api_key"));
}

#[test]
fn a_server_declaring_nothing_secret_has_no_credential_kind() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/open",
        "remotes": [{ "url": "https://api.test/mcp" }],
    }))
    .unwrap();

    assert_eq!(server.into_summary().auth_kind, None);
}

#[test]
fn a_row_is_never_badged_by_the_adapter() {
    // Badging is curation's job, from its own list.
    let server: OfficialServer =
        serde_json::from_value(json!({ "name": "com.notion/mcp" })).unwrap();

    assert!(!server.into_summary().official);
}

// ---------------------------------------------------------------------------
// Detail conversion
// ---------------------------------------------------------------------------

#[test]
fn a_remote_becomes_an_http_connection_with_its_endpoint() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{ "url": "https://api.test/mcp" }],
    }))
    .unwrap();

    let detail = server.into_detail();
    assert_eq!(detail.connections.len(), 1);
    assert_eq!(detail.connections[0].r#type, "http");
    assert_eq!(
        detail.connections[0].deployment_url.as_deref(),
        Some("https://api.test/mcp")
    );
}

#[test]
fn a_package_becomes_a_subprocess_connection_with_no_endpoint() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{ "registryType": "npm", "identifier": "some-server" }],
    }))
    .unwrap();

    let detail = server.into_detail();
    assert_eq!(detail.connections.len(), 1);
    assert_eq!(detail.connections[0].r#type, "stdio");
    assert_eq!(detail.connections[0].deployment_url, None);
}

#[test]
fn a_server_offering_both_gets_a_connection_for_each() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{ "url": "https://api.test/mcp" }],
        "packages": [{ "registryType": "npm", "identifier": "some-server" }],
    }))
    .unwrap();

    assert_eq!(server.into_detail().connections.len(), 2);
}

// ---------------------------------------------------------------------------
// Input schemas
// ---------------------------------------------------------------------------

#[test]
fn declared_headers_become_an_input_schema() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{
            "url": "https://api.test/mcp",
            "headers": [
                { "name": "X-Api-Key", "description": "your key", "isSecret": true, "isRequired": true },
                { "name": "X-Org", "description": "your organisation" },
            ],
        }],
    }))
    .unwrap();

    let schema = server.into_detail().connections[0]
        .config_schema
        .clone()
        .expect("a schema");

    assert_eq!(
        schema["properties"]["X-Api-Key"]["description"],
        json!("your key")
    );
    assert_eq!(schema["properties"]["X-Api-Key"]["x-secret"], json!(true));
    assert_eq!(schema["required"], json!(["X-Api-Key"]));
    // Not marked secret, so no masking marker.
    assert!(schema["properties"]["X-Org"].get("x-secret").is_none());
}

#[test]
fn a_remote_declaring_no_headers_has_no_schema() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{ "url": "https://api.test/mcp" }],
    }))
    .unwrap();

    assert_eq!(server.into_detail().connections[0].config_schema, None);
}

#[test]
fn an_unnamed_input_is_skipped() {
    // It could neither be prompted for nor sent.
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "remotes": [{
            "url": "https://api.test/mcp",
            "headers": [{ "name": "", "isRequired": true }],
        }],
    }))
    .unwrap();

    assert_eq!(server.into_detail().connections[0].config_schema, None);
}

#[test]
fn declared_environment_variables_become_an_input_schema() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "npm",
            "identifier": "server",
            "environmentVariables": [
                { "name": "API_KEY", "isSecret": true, "isRequired": true },
            ],
        }],
    }))
    .unwrap();

    let schema = server.into_detail().connections[0]
        .config_schema
        .clone()
        .expect("a schema");

    assert_eq!(schema["properties"]["API_KEY"]["x-secret"], json!(true));
    assert_eq!(schema["required"], json!(["API_KEY"]));
}

#[test]
fn a_registry_supplied_schema_is_used_when_no_variables_are_declared() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "npm",
            "identifier": "server",
            "configSchema": { "properties": { "region": {} } },
        }],
    }))
    .unwrap();

    let schema = server.into_detail().connections[0]
        .config_schema
        .clone()
        .expect("a schema");

    assert!(schema["properties"].get("region").is_some());
}

// ---------------------------------------------------------------------------
// Launch examples
// ---------------------------------------------------------------------------

#[test]
fn a_python_package_is_launched_with_uvx() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{ "registryType": "pypi", "identifier": "some-server" }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["command"], json!("uvx"));
    assert_eq!(example["args"], json!(["some-server"]));
}

#[test]
fn a_node_package_is_launched_with_npx_and_the_yes_flag() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{ "registryType": "npm", "identifier": "some-server" }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["command"], json!("npx"));
    assert_eq!(example["args"], json!(["-y", "some-server"]));
}

#[test]
fn a_node_package_with_its_own_arguments_does_not_get_the_yes_flag() {
    // A package declaring arguments may well be passing its own flags first.
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "npm",
            "identifier": "some-server",
            "runtimeArguments": [{ "value": "--stdio" }],
        }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["args"], json!(["--stdio", "some-server"]));
}

#[test]
fn a_declared_runtime_hint_wins_over_the_default_launcher() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{
            "registryType": "pypi",
            "identifier": "some-server",
            "runtimeHint": "pipx",
        }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["command"], json!("pipx"));
}

#[test]
fn an_unrecognised_package_kind_is_launched_as_a_node_one() {
    // Most of the ecosystem is Node, so it is the least surprising guess when
    // the registry does not say.
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "com.vendor/server",
        "packages": [{ "registryType": "brew", "identifier": "some-server" }],
    }))
    .unwrap();

    let example = server.into_detail().connections[0]
        .example_config
        .clone()
        .expect("an example");

    assert_eq!(example["command"], json!("npx"));
}

// ---------------------------------------------------------------------------
// Cursors and page counts
// ---------------------------------------------------------------------------

#[test]
fn a_response_with_more_results_reports_one_page_beyond_the_current_one() {
    assert_eq!(page_bound(1, true), 2);
    assert_eq!(page_bound(7, true), 8);
}

#[test]
fn a_response_ending_the_results_reports_the_current_page() {
    assert_eq!(page_bound(1, false), 1);
    assert_eq!(page_bound(7, false), 7);
}

#[test]
fn the_page_bound_does_not_overflow() {
    assert_eq!(page_bound(u32::MAX, true), u32::MAX);
}

#[test]
fn a_cursor_is_read_from_the_metadata() {
    let response = list_response(&json!([]), Some("token-1"));
    assert_eq!(response.next_cursor(), Some("token-1"));
}

#[test]
fn an_empty_cursor_counts_as_no_cursor() {
    // The registry sends one at the end of a result set, and treating it as a
    // cursor would page forever.
    let response = list_response(&json!([]), Some(""));
    assert_eq!(response.next_cursor(), None);
}

#[test]
fn a_response_with_no_metadata_has_no_cursor() {
    assert_eq!(list_response(&json!([]), None).next_cursor(), None);
}

#[test]
fn a_cache_key_separates_query_page_and_size() {
    assert_eq!(
        search_cache_key("weather", 2, 50),
        "mcp_official:search:weather:2:50"
    );
    assert_ne!(search_cache_key("a", 1, 20), search_cache_key("a", 2, 20));
    assert_ne!(search_cache_key("a", 1, 20), search_cache_key("a", 1, 50));
    assert_ne!(search_cache_key("a", 1, 20), search_cache_key("b", 1, 20));
}
