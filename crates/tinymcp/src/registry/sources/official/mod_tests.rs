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
use super::{lead_with_curated, page_bound, search_cache_key};

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
    assert_eq!(list_response(&json!([]), None).into_summaries().len(), 0);
    let empty: OfficialListResponse = serde_json::from_value(json!({})).unwrap();
    assert_eq!(empty.into_summaries().len(), 0);
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

    assert_eq!(response.into_summaries().len(), 0);
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

    assert_eq!(response.into_summaries().len(), 0);
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
// Versions
// ---------------------------------------------------------------------------

/// An envelope for one version of `name`, marked latest or not.
fn version(name: &str, title: &str, latest: Option<bool>) -> Value {
    let mut row = json!({
        "server": {
            "name": name,
            "title": title,
            "remotes": [{ "url": "https://api.test/mcp" }],
        },
    });
    if let Some(latest) = latest {
        row["_meta"] = json!({
            "io.modelcontextprotocol.registry/official": { "status": "active", "isLatest": latest },
        });
    }
    row
}

#[test]
fn a_page_listing_every_version_keeps_one_row_per_server() {
    let rows = recorded(EVERY_VERSION_PAGE);
    let names: std::collections::BTreeSet<&str> =
        rows.iter().map(|row| row.qualified_name.as_str()).collect();

    assert_eq!(rows.len(), 11);
    assert_eq!(names.len(), rows.len(), "every server appears once");
}

#[test]
fn the_version_marked_latest_is_the_one_kept() {
    let rows = recorded(EVERY_VERSION_PAGE);
    let brainy = rows
        .iter()
        .find(|row| row.qualified_name == "ae.brainy/grocery-prices")
        .expect("listed");

    assert_eq!(brainy.display_name, "Brainy Prices — UAE Cost of Living");
}

#[test]
fn a_recorded_latest_page_keeps_every_row() {
    assert_eq!(recorded(LATEST_PAGE).len(), 20);
}

#[test]
fn a_page_keeps_the_order_of_each_server_first_row() {
    let rows = recorded(EVERY_VERSION_PAGE);

    assert_eq!(rows[0].qualified_name, "ac.inference.sh/mcp");
    assert_eq!(rows[1].qualified_name, "ac.snag/snag");
}

#[test]
fn a_latest_row_is_not_displaced_by_a_later_one() {
    let response = list_response(
        &json!([
            version("same/server", "Latest", Some(true)),
            version("same/server", "Listed after", Some(false)),
        ]),
        None,
    );

    assert_eq!(response.into_summaries()[0].display_name, "Latest");
}

#[test]
fn without_a_latest_mark_the_last_listed_version_is_kept() {
    let response = list_response(
        &json!([
            version("same/server", "Older", None),
            version("same/server", "Newer", None),
        ]),
        None,
    );

    let rows = response.into_summaries();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].display_name, "Newer");
}

#[test]
fn a_versions_document_yields_the_version_marked_latest() {
    let document = json!({
        "servers": [
            version("same/server", "Older", Some(false)),
            version("same/server", "Latest", Some(true)),
        ],
    });

    let latest = super::types::latest_version(&document).expect("a version");
    assert_eq!(latest["title"], "Latest");
}

#[test]
fn a_versions_document_with_no_mark_yields_its_first_version() {
    let document = json!({
        "servers": [version("same/server", "First", None), version("same/server", "Second", None)],
    });

    let latest = super::types::latest_version(&document).expect("a version");
    assert_eq!(latest["title"], "First");
}

#[test]
fn a_versions_document_with_no_servers_yields_nothing() {
    assert!(super::types::latest_version(&json!({})).is_none());
    assert!(super::types::latest_version(&json!({ "servers": [] })).is_none());
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
// Icons
// ---------------------------------------------------------------------------

/// A page recorded from the live registry with `version=latest`.
const LATEST_PAGE: &str = include_str!("fixtures/latest_page.json");

/// A page recorded from the live registry without `version=latest`, so it
/// lists every version of each server.
const EVERY_VERSION_PAGE: &str = include_str!("fixtures/every_version_page.json");

/// The rows of a recorded page.
fn recorded(page: &str) -> Vec<tinymcp_bus::RegistryServerSummary> {
    serde_json::from_str::<OfficialListResponse>(page)
        .expect("the recorded page decodes")
        .into_summaries()
}

/// The icon a recorded row ended up with.
fn icon_of<'a>(rows: &'a [tinymcp_bus::RegistryServerSummary], name: &str) -> Option<&'a str> {
    rows.iter()
        .find(|row| row.qualified_name == name)
        .unwrap_or_else(|| panic!("{name} is on the recorded page"))
        .icon_url
        .as_deref()
}

#[test]
fn a_recorded_page_carries_its_declared_icons() {
    let rows = recorded(LATEST_PAGE);

    assert_eq!(
        icon_of(&rows, "ae.projectory/mcp"),
        Some("https://projectory.ae/apple-touch-icon.png")
    );
    assert_eq!(
        icon_of(&rows, "africa.ugc/ugc"),
        Some("https://ugc.africa/icon-512.png")
    );
    assert_eq!(icon_of(&rows, "ac.snag/snag"), None);
}

#[test]
fn a_raster_icon_is_preferred_over_an_svg_listed_first() {
    let rows = recorded(LATEST_PAGE);

    assert_eq!(
        icon_of(&rows, "ae.plantguide/dubai-gardening"),
        Some("https://plantguide.ae/icons/icon-512.png")
    );
}

#[test]
fn an_svg_is_used_when_it_is_the_only_icon() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "svg/only",
        "icons": [{ "src": "https://svg.test/mark.svg", "mimeType": "image/svg+xml" }],
    }))
    .unwrap();

    assert_eq!(
        server.best_icon().as_deref(),
        Some("https://svg.test/mark.svg")
    );
}

#[test]
fn an_svg_is_recognised_by_its_extension_when_no_type_is_declared() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "mixed/icons",
        "icons": [
            { "src": "https://icons.test/mark.SVG?v=2" },
            { "src": "https://icons.test/mark.webp" },
        ],
    }))
    .unwrap();

    assert_eq!(
        server.best_icon().as_deref(),
        Some("https://icons.test/mark.webp")
    );
}

#[test]
fn a_blank_icon_source_is_skipped() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "blank/icon",
        "icons": [
            { "src": "  ", "mimeType": "image/png" },
            { "src": "https://icons.test/mark.svg", "mimeType": "image/svg+xml" },
        ],
    }))
    .unwrap();

    assert_eq!(
        server.best_icon().as_deref(),
        Some("https://icons.test/mark.svg")
    );
}

#[test]
fn the_legacy_icon_url_still_answers_when_no_icons_are_declared() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "legacy/icon",
        "iconUrl": "https://legacy.test/icon.png",
    }))
    .unwrap();

    assert_eq!(
        server.best_icon().as_deref(),
        Some("https://legacy.test/icon.png")
    );
}

#[test]
fn declared_icons_win_over_the_legacy_icon_url() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "both/icons",
        "iconUrl": "https://legacy.test/icon.png",
        "icons": [{ "src": "https://icons.test/icon.png", "mimeType": "image/png" }],
    }))
    .unwrap();

    assert_eq!(
        server.best_icon().as_deref(),
        Some("https://icons.test/icon.png")
    );
}

#[test]
fn a_server_declaring_no_icon_has_none() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "no/icon",
        "iconUrl": " ",
    }))
    .unwrap();

    assert_eq!(server.best_icon(), None);
}

#[test]
fn a_detail_record_carries_the_preferred_icon() {
    let server: OfficialServer = serde_json::from_value(json!({
        "name": "detail/icon",
        "icons": [
            { "src": "https://icons.test/mark.svg", "mimeType": "image/svg+xml" },
            { "src": "https://icons.test/mark.png", "mimeType": "image/png" },
        ],
        "remotes": [{ "url": "https://api.test/mcp" }],
    }))
    .unwrap();

    assert_eq!(
        server.into_detail().icon_url.as_deref(),
        Some("https://icons.test/mark.png")
    );
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
        "mcp_official:search:latest:weather:2:50"
    );
    assert_ne!(search_cache_key("a", 1, 20), search_cache_key("a", 2, 20));
    assert_ne!(search_cache_key("a", 1, 20), search_cache_key("a", 1, 50));
    assert_ne!(search_cache_key("a", 1, 20), search_cache_key("b", 1, 20));
}

// ---------------------------------------------------------------------------
// The adapter against a loopback registry
// ---------------------------------------------------------------------------
//
// Everything above is shape conversion, which needs no server. What follows
// exercises the parts that only exist because the registry pages by opaque
// cursor: the walk, the two caches, and what happens when the chain ends early.
// Those are the paths a fixture cannot reach.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use axum::routing::get;
use parking_lot::Mutex;

use super::super::types::SourcePage;
use super::{MAX_CURSOR_WALK_PAGES, McpOfficialRegistry};
use crate::error::Error;
use crate::registry::Store;
use tinymcp_bus::McpRegistryAuthConfig;

/// What the mock registry saw.
#[derive(Debug, Default)]
struct Seen {
    pages: AtomicUsize,
    cursors: Mutex<Vec<Option<String>>>,
    versions: Mutex<Vec<Option<String>>>,
    authorization: Mutex<Option<String>>,
    detail_path: Mutex<Option<String>>,
}

/// One query parameter off a request URI.
///
/// Read by hand: the workspace takes `axum` without default features, and the
/// query extractor is not among the few this suite needs enabled.
fn param(uri: &Uri, name: &str) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.replace('+', " "))
    })
}

/// Binds a loopback port and serves `app`, returning its base URL.
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// A registry whose result set runs to `pages` pages of one row each.
///
/// Cursors are the string form of the next page number, which is opaque enough
/// for the adapter — it never interprets one — and legible in a failure.
async fn paged_registry(pages: usize) -> (String, Arc<Seen>) {
    let seen = Arc::new(Seen::default());

    let app = Router::new()
        .route(
            "/v0/servers",
            get(
                move |State(seen): State<Arc<Seen>>, headers: HeaderMap, uri: Uri| async move {
                    seen.pages.fetch_add(1, Ordering::SeqCst);
                    *seen.authorization.lock() = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .map(ToString::to_string);

                    let cursor = param(&uri, "cursor");
                    seen.cursors.lock().push(cursor.clone());
                    seen.versions.lock().push(param(&uri, "version"));

                    let page: usize = cursor
                        .as_deref()
                        .and_then(|cursor| cursor.parse().ok())
                        .unwrap_or(1);

                    let mut body = json!({
                        "servers": [envelope(&format!("@acme/server-{page}"))],
                    });
                    if page < pages {
                        body["metadata"] = json!({ "nextCursor": (page + 1).to_string() });
                    }

                    axum::Json(body)
                },
            ),
        )
        .route(
            "/v0/servers/{*rest}",
            get(|State(seen): State<Arc<Seen>>, uri: Uri| async move {
                seen.pages.fetch_add(1, Ordering::SeqCst);
                // The path carries `{name}/versions`; the name is everything
                // before the trailing segment, and it arrives percent-encoded
                // as one segment — which is the point of encoding it.
                let path = uri.path().trim_start_matches("/v0/servers/");
                let name = path.trim_end_matches("/versions").replace("%2F", "/");
                *seen.detail_path.lock() = Some(uri.path().to_string());
                axum::Json(json!({ "servers": [envelope(&name)] }))
            }),
        )
        .with_state(Arc::clone(&seen));

    (serve(app).await, seen)
}

/// A registry that answers every request with `status` and `body`.
async fn failing_registry(status: u16, body: &'static str) -> String {
    let app = Router::new().fallback(get(move || async move {
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            body.to_string(),
        )
    }));

    serve(app).await
}

/// Credentials naming `base` as the registry.
fn auth_at(base: &str) -> McpRegistryAuthConfig {
    McpRegistryAuthConfig {
        mcp_official_base: Some(base.to_string()),
        ..McpRegistryAuthConfig::default()
    }
}

/// An empty cursor map.
fn cursors() -> Mutex<HashMap<(String, u32, u32), String>> {
    Mutex::new(HashMap::new())
}

/// An empty store to cache into.
fn store() -> Store {
    Store::open_in_memory().expect("the store opens")
}

/// The adapter under test.
fn adapter() -> McpOfficialRegistry {
    McpOfficialRegistry::new().expect("the adapter builds")
}

// ---------------------------------------------------------------------------
// Searching
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_first_page_is_fetched_without_a_cursor() {
    let (base, seen) = paged_registry(3).await;

    let SourcePage { servers, .. } = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .expect("the search succeeds");

    assert_eq!(servers.len(), 1);
    assert_eq!(seen.cursors.lock().as_slice(), &[None]);
}

#[tokio::test]
async fn every_page_asks_for_the_latest_version_only() {
    let (base, seen) = paged_registry(3).await;

    adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 2, 20)
        .await
        .unwrap();

    assert_eq!(
        seen.versions.lock().as_slice(),
        &[Some("latest".to_string()), Some("latest".to_string())]
    );
}

#[tokio::test]
async fn a_page_with_more_behind_it_reports_one_page_beyond() {
    // A bound, not a total: knowing the true count would mean walking the whole
    // chain, which is the cost this design exists to avoid. One page beyond is
    // what a caller needs to decide whether to offer a "next" control.
    let (base, _seen) = paged_registry(3).await;

    let SourcePage { total_pages, .. } = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(total_pages, 2);
}

#[tokio::test]
async fn the_last_page_reports_itself_as_the_last() {
    let (base, _seen) = paged_registry(1).await;

    let SourcePage { total_pages, .. } = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(total_pages, 1);
}

#[tokio::test]
async fn a_configured_token_is_sent_as_a_bearer() {
    let (base, seen) = paged_registry(1).await;
    let auth = McpRegistryAuthConfig {
        mcp_official_token: Some("tok-test".into()),
        ..auth_at(&base)
    };

    adapter()
        .search(&store(), &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(
        seen.authorization.lock().as_deref(),
        Some("Bearer tok-test")
    );
}

#[tokio::test]
async fn no_token_means_no_authorization_header() {
    let (base, seen) = paged_registry(1).await;

    adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(*seen.authorization.lock(), None);
}

// ---------------------------------------------------------------------------
// Paging over cursors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_warm_map_reaches_the_next_page_in_one_request() {
    // The whole point of keeping the map: paging sequentially must not re-walk
    // the chain each time.
    let (base, seen) = paged_registry(5).await;
    let auth = auth_at(&base);
    let cursors = cursors();
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors, "", 1, 20)
        .await
        .unwrap();
    let before = seen.pages.load(Ordering::SeqCst);

    adapter
        .search(&store, &auth, &cursors, "", 2, 20)
        .await
        .unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst) - before, 1);
}

#[tokio::test]
async fn a_cold_map_walks_forward_to_reach_a_deep_page() {
    // A link straight to page four, or the first search after a restart.
    let (base, seen) = paged_registry(5).await;

    let SourcePage { servers, .. } = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 4, 20)
        .await
        .expect("the walk reaches page four");

    assert_eq!(servers[0].qualified_name, "@acme/server-4");
    // Pages one through three to learn the cursors, then page four itself.
    assert_eq!(seen.pages.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_walk_fills_the_map_for_the_pages_it_passed() {
    // Otherwise walking to page four then asking for page three would walk
    // again, and paging backwards would cost more than paging forwards.
    let (base, seen) = paged_registry(5).await;
    let auth = auth_at(&base);
    let cursors = cursors();
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors, "", 4, 20)
        .await
        .unwrap();
    let before = seen.pages.load(Ordering::SeqCst);

    adapter
        .search(&store, &auth, &cursors, "", 3, 20)
        .await
        .unwrap();

    // Served from the stored page bodies the walk cached.
    assert_eq!(seen.pages.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn a_walk_reads_the_stored_cache_before_the_network() {
    // A cold in-memory map after a restart must not mean a cold network: the
    // page bodies from the previous run are still on disk.
    let (base, seen) = paged_registry(5).await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 4, 20)
        .await
        .unwrap();
    let before = seen.pages.load(Ordering::SeqCst);

    // A fresh map, as after a restart, against the same store.
    adapter
        .search(&store, &auth, &cursors(), "", 4, 20)
        .await
        .unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn a_page_past_the_end_of_the_chain_comes_back_empty() {
    // Rather than an error: the chain simply ran out, and an empty result
    // naming this page as the last is what stops a caller paging further.
    let (base, _seen) = paged_registry(2).await;

    let SourcePage {
        servers,
        total_pages,
        ..
    } = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 5, 20)
        .await
        .expect("running out of pages is not a failure");

    assert_eq!(servers.len(), 0);
    assert_eq!(total_pages, 5);
}

#[tokio::test]
async fn a_page_beyond_the_walk_limit_is_refused() {
    // A single request would otherwise fan into hundreds upstream, which is a
    // denial of service aimed at someone else.
    let (base, seen) = paged_registry(200).await;
    let target = MAX_CURSOR_WALK_PAGES + 1;

    let error = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", target, 20)
        .await
        .expect_err("beyond the walk limit");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("page sequentially"), "{error}");
    assert_eq!(
        seen.pages.load(Ordering::SeqCst),
        0,
        "nothing was requested"
    );
}

// ---------------------------------------------------------------------------
// The response cache
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_repeated_search_is_served_from_the_stored_cache() {
    let (base, seen) = paged_registry(3).await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    let SourcePage {
        servers,
        total_pages,
        ..
    } = adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst), 1);
    assert_eq!(servers.len(), 1);
    assert_eq!(total_pages, 2);
}

#[tokio::test]
async fn a_cache_hit_still_records_the_cursor_it_carried() {
    // Otherwise a hit on page one would leave the map cold, and page two would
    // walk from the start — the cache would make paging slower.
    let (base, seen) = paged_registry(5).await;
    let auth = auth_at(&base);
    let store = store();
    let warm = cursors();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    // A fresh map reading the warm store, then straight on to page two.
    adapter
        .search(&store, &auth, &warm, "", 1, 20)
        .await
        .unwrap();
    let before = seen.pages.load(Ordering::SeqCst);

    adapter
        .search(&store, &auth, &warm, "", 2, 20)
        .await
        .unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst) - before, 1);
}

// ---------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_detail_lookup_takes_the_newest_version() {
    // The registry has no single-server endpoint, so a lookup reads the version
    // list, which leads with the newest.
    let (base, _seen) = paged_registry(1).await;

    let detail = adapter()
        .get(&store(), &auth_at(&base), "@acme/weather")
        .await
        .expect("the lookup succeeds");

    assert_eq!(detail.qualified_name, "@acme/weather");
}

#[tokio::test]
async fn a_detail_lookup_takes_the_version_marked_latest() {
    let app = Router::new().fallback(get(|| async {
        axum::Json(json!({
            "servers": [
                version("@acme/weather", "Weather 1", Some(false)),
                version("@acme/weather", "Weather 2", Some(true)),
            ],
        }))
    }));
    let base = serve(app).await;

    let detail = adapter()
        .get(&store(), &auth_at(&base), "@acme/weather")
        .await
        .expect("the lookup succeeds");

    assert_eq!(detail.display_name, "Weather 2");
}

#[tokio::test]
async fn a_repeated_detail_lookup_is_served_from_the_cache() {
    let (base, seen) = paged_registry(1).await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter.get(&store, &auth, "@acme/weather").await.unwrap();
    let detail = adapter.get(&store, &auth, "@acme/weather").await.unwrap();

    assert_eq!(seen.pages.load(Ordering::SeqCst), 1);
    assert_eq!(detail.qualified_name, "@acme/weather");
}

#[tokio::test]
async fn a_name_containing_a_slash_is_sent_as_one_escaped_segment() {
    // `@acme/weather` is one name. Sent raw it would be two path segments and
    // reach a different route, or none — and the trailing `/versions` the
    // adapter appends would no longer be the last segment.
    let (base, seen) = paged_registry(1).await;

    adapter()
        .get(&store(), &auth_at(&base), "@acme/weather")
        .await
        .unwrap();

    assert_eq!(
        seen.detail_path.lock().as_deref(),
        Some("/v0/servers/@acme%2Fweather/versions")
    );
}

#[tokio::test]
async fn a_registry_listing_no_versions_reports_an_unknown_server() {
    let app = Router::new().fallback(get(|| async { axum::Json(json!({ "servers": [] })) }));
    let base = serve(app).await;

    let error = adapter()
        .get(&store(), &auth_at(&base), "@acme/nothing")
        .await
        .expect_err("no versions");

    match error {
        Error::UnknownServer { server } => assert_eq!(server, "@acme/nothing"),
        other => panic!("expected an unknown-server error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_versions_body_that_is_not_json_is_reported_as_malformed() {
    let base = failing_registry(200, "not json").await;

    let error = adapter()
        .get(&store(), &auth_at(&base), "@acme/weather")
        .await
        .expect_err("unparseable");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_upstream_failure_status_is_reported_with_its_body() {
    let base = failing_registry(503, "maintenance").await;

    let error = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .expect_err("503");

    match error {
        Error::Http { status, body, .. } => {
            assert_eq!(status, 503);
            assert_eq!(body, "maintenance");
        }
        other => panic!("expected an http error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_long_failure_body_is_truncated() {
    let base = failing_registry(
        500,
        concat!(
            "0123456789012345678901234567890123456789012345678901234567890123456789",
            "0123456789012345678901234567890123456789012345678901234567890123456789",
            "0123456789012345678901234567890123456789012345678901234567890123456789",
            "0123456789012345678901234567890123456789",
        ),
    )
    .await;

    let error = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .expect_err("500");

    match error {
        Error::Http { body, .. } => assert_eq!(body.len(), 200),
        other => panic!("expected an http error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unreachable_registry_is_reported_as_a_transport_failure() {
    let error = adapter()
        .search(
            &store(),
            &auth_at("http://127.0.0.1:1"),
            &cursors(),
            "",
            1,
            20,
        )
        .await
        .expect_err("unreachable");

    assert!(matches!(error, Error::Transport { .. }), "{error:?}");
}

#[tokio::test]
async fn a_list_body_that_does_not_decode_is_reported_as_malformed() {
    let base = failing_registry(200, "[1, 2, 3]").await;

    let error = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .expect_err("unparseable");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Time budgets
// ---------------------------------------------------------------------------

use std::time::Duration;

use super::super::types::{RegistryOperation, RegistryTimeouts};

/// A registry that holds every request for `delay` before answering.
async fn slow_registry(delay: Duration) -> (String, Arc<Seen>) {
    let seen = Arc::new(Seen::default());

    let app = Router::new()
        .fallback(get(move |State(seen): State<Arc<Seen>>| async move {
            seen.pages.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            axum::Json(json!({ "servers": [envelope("@acme/slow")] }))
        }))
        .with_state(Arc::clone(&seen));

    (serve(app).await, seen)
}

/// Budgets short enough for a test, with `search` shorter than the rest.
fn short_budgets() -> RegistryTimeouts {
    RegistryTimeouts {
        connect: Duration::from_secs(5),
        browse: Duration::from_secs(5),
        search: Duration::from_millis(100),
        detail: Duration::from_millis(100),
        cooldown: Duration::from_secs(60),
    }
}

/// An adapter with [`short_budgets`].
fn impatient_adapter() -> McpOfficialRegistry {
    McpOfficialRegistry::with_timeouts(short_budgets()).expect("the adapter builds")
}

#[tokio::test]
async fn a_stalled_search_fails_within_its_budget_as_a_registry_timeout() {
    let (base, _seen) = slow_registry(Duration::from_secs(5)).await;
    let started = std::time::Instant::now();

    let error = impatient_adapter()
        .search(&store(), &auth_at(&base), &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("the search stalls");

    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    match error {
        Error::RegistryTimeout {
            operation, timeout, ..
        } => {
            assert_eq!(operation, RegistryOperation::Search);
            assert_eq!(timeout, Duration::from_millis(100));
        }
        other => panic!("expected a registry timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn a_browse_has_its_own_longer_budget() {
    let (base, _seen) = slow_registry(Duration::from_millis(300)).await;

    let SourcePage { servers, .. } = impatient_adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .expect("the browse budget covers the delay");

    assert_eq!(servers.len(), 1);
}

#[tokio::test]
async fn a_stalled_detail_lookup_is_a_detail_timeout() {
    let (base, _seen) = slow_registry(Duration::from_secs(5)).await;

    let error = impatient_adapter()
        .get(&store(), &auth_at(&base), "@acme/slow")
        .await
        .expect_err("the lookup stalls");

    assert!(
        matches!(
            error,
            Error::RegistryTimeout {
                operation: RegistryOperation::Detail,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.is_registry_unavailable());
}

// ---------------------------------------------------------------------------
// Serving from the cache when the registry cannot answer
// ---------------------------------------------------------------------------

use axum::response::IntoResponse as _;
use tinymcp_bus::RegistryFreshness;

/// The registry answers normally.
const UP: usize = 0;
/// The registry answers 503.
const FAILING: usize = 1;
/// The registry holds every request past any test budget.
const STALLED: usize = 2;
/// The registry answers 200 with a body that is not a list.
const GARBLED: usize = 3;

/// A registry whose behaviour a test switches between [`UP`], [`FAILING`],
/// [`STALLED`] and [`GARBLED`].
#[derive(Debug, Default)]
struct Switchable {
    mode: AtomicUsize,
    requests: AtomicUsize,
}

impl Switchable {
    fn set(&self, mode: usize) {
        self.mode.store(mode, Ordering::SeqCst);
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// A switchable registry: browsing answers the recorded latest page, and a
/// search for `q` answers one row named `@acme/q`.
async fn switchable_registry() -> (String, Arc<Switchable>) {
    let state = Arc::new(Switchable::default());

    let app = Router::new()
        .route(
            "/v0/servers",
            get(
                |State(state): State<Arc<Switchable>>, uri: Uri| async move {
                    state.requests.fetch_add(1, Ordering::SeqCst);
                    match state.mode.load(Ordering::SeqCst) {
                        FAILING => {
                            (axum::http::StatusCode::SERVICE_UNAVAILABLE, "down").into_response()
                        }
                        STALLED => {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            axum::Json(json!({ "servers": [] })).into_response()
                        }
                        GARBLED => "[1, 2, 3]".into_response(),
                        _ => match param(&uri, "search") {
                            Some(query) => axum::Json(json!({
                                "servers": [envelope(&format!("@acme/{query}"))],
                            }))
                            .into_response(),
                            None => LATEST_PAGE.into_response(),
                        },
                    }
                },
            ),
        )
        .with_state(Arc::clone(&state));

    (serve(app).await, state)
}

/// Moves every cached entry past the cache lifetime.
fn expire_cache(store: &Store) {
    store.age_collection(
        crate::storage::REGISTRY_CACHE,
        &["cached_ms"],
        24 * 60 * 60 * 1_000,
    );
}

/// Budgets with every listing short, and `cooldown` as given.
fn budgets_with_cooldown(cooldown: Duration) -> RegistryTimeouts {
    RegistryTimeouts {
        browse: Duration::from_millis(200),
        cooldown,
        ..short_budgets()
    }
}

/// An adapter with [`budgets_with_cooldown`].
fn adapter_with_cooldown(cooldown: Duration) -> McpOfficialRegistry {
    McpOfficialRegistry::with_timeouts(budgets_with_cooldown(cooldown)).expect("the adapter builds")
}

/// The search budget for tests that time a search made during the cooldown.
///
/// Such a search must not wait on the network at all. The request count is
/// what proves nothing was sent; the wall clock only adds that the adapter did
/// not wait out the budget either. With the 100ms of [`short_budgets`] those
/// two are indistinguishable from scheduler noise on a shared CI runner, so
/// these tests use a budget long enough that half of it is a bound no idle
/// call comes near, and a call that waited the budget cannot meet.
const COOLDOWN_SEARCH_BUDGET: Duration = Duration::from_secs(1);

/// An adapter cooling down for a minute, with [`COOLDOWN_SEARCH_BUDGET`].
fn cooling_adapter() -> McpOfficialRegistry {
    McpOfficialRegistry::with_timeouts(RegistryTimeouts {
        search: COOLDOWN_SEARCH_BUDGET,
        ..budgets_with_cooldown(Duration::from_secs(60))
    })
    .expect("the adapter builds")
}

#[tokio::test]
async fn a_live_answer_is_reported_as_live() {
    let (base, _state) = switchable_registry().await;

    let page = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "", 1, 20)
        .await
        .unwrap();

    assert_eq!(page.freshness, RegistryFreshness::Live);
    assert_eq!(page.servers.len(), 20);
}

#[tokio::test]
async fn a_stalled_search_answers_with_its_earlier_result() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter_with_cooldown(Duration::from_secs(60));

    adapter
        .search(&store, &auth, &cursors(), "weather", 1, 20)
        .await
        .unwrap();
    expire_cache(&store);
    state.set(STALLED);

    let page = adapter
        .search(&store, &auth, &cursors(), "weather", 1, 20)
        .await
        .expect("the earlier result stands in");

    assert_eq!(page.freshness, RegistryFreshness::Cached);
    assert_eq!(page.servers[0].qualified_name, "@acme/weather");
}

#[tokio::test]
async fn a_failing_browse_answers_with_its_earlier_page_and_cursor() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let warm = cursors();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    expire_cache(&store);
    state.set(FAILING);

    let page = adapter
        .search(&store, &auth, &warm, "", 1, 20)
        .await
        .expect("the earlier page stands in");

    assert_eq!(page.freshness, RegistryFreshness::Cached);
    assert_eq!(page.servers.len(), 20);
    assert_eq!(page.total_pages, 2);
    assert!(
        warm.lock().contains_key(&(String::new(), 20, 1)),
        "the stale page's cursor was recorded"
    );
}

#[tokio::test]
async fn a_stalled_search_with_no_earlier_result_matches_cached_catalog_pages() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter_with_cooldown(Duration::from_secs(60));

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    state.set(STALLED);

    let page = adapter
        .search(&store, &auth, &cursors(), "Dubai", 1, 20)
        .await
        .expect("cached catalog rows stand in");

    let names: Vec<&str> = page
        .servers
        .iter()
        .map(|row| row.qualified_name.as_str())
        .collect();
    assert_eq!(page.freshness, RegistryFreshness::LocalFallback);
    assert_eq!(page.total_pages, 1);
    assert_eq!(
        names,
        [
            "ae.datadubai/dubai-real-estate",
            "ae.plantguide/dubai-gardening",
            "ae.propick/propick",
        ],
        "name and title matches lead description matches"
    );
}

#[tokio::test]
async fn a_local_match_needs_every_word_of_the_query() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    state.set(FAILING);

    let page = adapter
        .search(&store, &auth, &cursors(), "dubai gardening", 1, 20)
        .await
        .unwrap();

    assert_eq!(page.servers.len(), 1);
    assert_eq!(
        page.servers[0].qualified_name,
        "ae.plantguide/dubai-gardening"
    );
}

#[tokio::test]
async fn a_local_match_is_capped_at_the_page_size() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    state.set(FAILING);

    let page = adapter
        .search(&store, &auth, &cursors(), "a", 1, 3)
        .await
        .unwrap();

    assert_eq!(page.servers.len(), 3);
}

#[tokio::test]
async fn a_later_search_page_has_no_local_fallback() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    state.set(FAILING);

    let error = adapter
        .search(&store, &auth, &cursors(), "dubai", 2, 20)
        .await
        .expect_err("only the first page falls back");

    assert!(
        matches!(error, Error::Http { status: 503, .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_search_matching_nothing_cached_reports_the_typed_error() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter_with_cooldown(Duration::from_secs(60));

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    state.set(STALLED);

    let error = adapter
        .search(&store, &auth, &cursors(), "zzqqxx", 1, 20)
        .await
        .expect_err("nothing cached matches");

    assert!(error.is_timeout(), "{error:?}");
    assert_eq!(error.wire_name(), tinymcp_bus::errors::REGISTRY_TIMEOUT);
}

#[tokio::test]
async fn a_search_with_an_empty_cache_reports_the_typed_error() {
    let (base, state) = switchable_registry().await;
    state.set(FAILING);

    let error = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("nothing cached");

    assert!(error.is_registry_unavailable(), "{error:?}");
}

#[tokio::test]
async fn a_body_that_does_not_decode_is_not_hidden_behind_the_cache() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    expire_cache(&store);
    state.set(GARBLED);

    let error = adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .expect_err("a malformed answer is not an outage");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_stalled_search_skips_the_network_for_the_cooldown() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = cooling_adapter();

    state.set(STALLED);
    adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("stalled");
    let after_first = state.requests();
    let started = std::time::Instant::now();

    let error = adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("still cooling down");

    assert_eq!(state.requests(), after_first, "nothing was sent");
    assert!(
        started.elapsed() < COOLDOWN_SEARCH_BUDGET / 2,
        "{:?}",
        started.elapsed()
    );
    match error {
        Error::RegistryTimeout {
            operation, timeout, ..
        } => {
            assert_eq!(operation, RegistryOperation::Search);
            assert_eq!(timeout, COOLDOWN_SEARCH_BUDGET);
        }
        other => panic!("expected a registry timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn a_cooling_down_search_still_answers_from_the_cache() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter_with_cooldown(Duration::from_secs(60));

    adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .unwrap();
    state.set(STALLED);
    adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("stalled");
    let after_first = state.requests();

    let page = adapter
        .search(&store, &auth, &cursors(), "dubai", 1, 20)
        .await
        .expect("cached catalog rows stand in");

    assert_eq!(state.requests(), after_first);
    assert_eq!(page.freshness, RegistryFreshness::LocalFallback);
}

#[tokio::test]
async fn a_search_cooldown_does_not_hold_back_browsing() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter_with_cooldown(Duration::from_secs(60));

    state.set(STALLED);
    adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("stalled");
    state.set(UP);

    let page = adapter
        .search(&store, &auth, &cursors(), "", 1, 20)
        .await
        .expect("browsing goes to the network");

    assert_eq!(page.freshness, RegistryFreshness::Live);
}

#[tokio::test]
async fn the_network_is_tried_again_once_the_cooldown_ends() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter_with_cooldown(Duration::ZERO);

    state.set(STALLED);
    adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("stalled");
    state.set(UP);

    let page = adapter
        .search(&store, &auth, &cursors(), "acme", 1, 20)
        .await
        .expect("the registry answers again");

    assert_eq!(page.freshness, RegistryFreshness::Live);
    assert_eq!(page.servers[0].qualified_name, "@acme/acme");
}

#[tokio::test]
async fn a_cooldown_is_scoped_to_the_registry_that_stalled() {
    let (stalled_base, stalled) = switchable_registry().await;
    let (healthy_base, _healthy) = switchable_registry().await;
    let store = store();
    let adapter = adapter_with_cooldown(Duration::from_secs(60));

    stalled.set(STALLED);
    adapter
        .search(
            &store,
            &auth_at(&stalled_base),
            &cursors(),
            "nonesuch",
            1,
            20,
        )
        .await
        .expect_err("stalled");

    let page = adapter
        .search(
            &store,
            &auth_at(&healthy_base),
            &cursors(),
            "nonesuch",
            1,
            20,
        )
        .await
        .expect("another registry is not cooling down");

    assert_eq!(page.freshness, RegistryFreshness::Live);
}

#[tokio::test]
async fn a_failure_status_starts_no_cooldown() {
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter_with_cooldown(Duration::from_secs(60));

    state.set(FAILING);
    adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("503");
    state.set(UP);

    let page = adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect("an answered failure is retried at once");

    assert_eq!(page.freshness, RegistryFreshness::Live);
}

#[test]
fn every_browse_page_shares_the_browse_prefix_and_no_search_page_does() {
    assert!(search_cache_key("", 1, 20).starts_with(super::BROWSE_CACHE_PREFIX));
    assert!(search_cache_key("", 7, 50).starts_with(super::BROWSE_CACHE_PREFIX));
    assert!(!search_cache_key("github", 1, 20).starts_with(super::BROWSE_CACHE_PREFIX));
}

#[test]
fn a_local_match_skips_cached_pages_that_do_not_decode() {
    let store = store();
    store
        .cache(&search_cache_key("", 1, 20), r#""not an envelope""#)
        .unwrap();
    store
        .cache(
            &search_cache_key("", 2, 20),
            &json!({ "servers": [envelope("@acme/weather")] }).to_string(),
        )
        .unwrap();

    let page = super::fallback::serve_cached(&store, &cursors(), "weather", 1, 20)
        .expect("the decodable page answers");

    assert_eq!(page.servers[0].qualified_name, "@acme/weather");
}

#[test]
fn a_blank_query_has_no_local_matches() {
    let store = store();
    store
        .cache(
            &search_cache_key("", 1, 20),
            &json!({ "servers": [envelope("@acme/weather")] }).to_string(),
        )
        .unwrap();

    assert!(super::fallback::serve_cached(&store, &cursors(), "   ", 1, 20).is_none());
}

// ---------------------------------------------------------------------------
// Matching cached server details
// ---------------------------------------------------------------------------

/// A store holding only the cached details of `names`, as a detail lookup
/// writes them.
async fn store_with_details(names: &[&str]) -> Store {
    let (base, _seen) = paged_registry(1).await;
    let auth = auth_at(&base);
    let store = store();
    let adapter = adapter();
    for name in names {
        adapter.get(&store, &auth, name).await.unwrap();
    }
    store
}

/// Caches `server` as the detail of the server it names.
fn cache_detail(store: &Store, server: &Value) {
    let name = server["name"].as_str().unwrap();
    store
        .cache(
            &format!("{}{name}", super::DETAIL_CACHE_PREFIX),
            &server.to_string(),
        )
        .unwrap();
}

fn names_of(page: &SourcePage) -> Vec<&str> {
    page.servers
        .iter()
        .map(|row| row.qualified_name.as_str())
        .collect()
}

#[tokio::test]
async fn a_stalled_search_matches_cached_server_details() {
    let store = store_with_details(&["com.notion/mcp", "io.github.acme/weather"]).await;
    let (base, state) = switchable_registry().await;
    state.set(STALLED);
    let started = std::time::Instant::now();

    let page = adapter_with_cooldown(Duration::from_secs(60))
        .search(&store, &auth_at(&base), &cursors(), "notion", 1, 20)
        .await
        .expect("the cached detail stands in");

    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(page.freshness, RegistryFreshness::LocalFallback);
    assert_eq!(page.total_pages, 1);
    assert_eq!(names_of(&page), ["com.notion/mcp"]);
}

#[tokio::test]
async fn a_cooling_down_search_answers_from_cached_server_details() {
    let store = store_with_details(&["com.notion/mcp"]).await;
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let adapter = cooling_adapter();

    state.set(STALLED);
    adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("stalled");
    let after_first = state.requests();
    let started = std::time::Instant::now();

    let page = adapter
        .search(&store, &auth, &cursors(), "Notion", 1, 20)
        .await
        .expect("the cached detail stands in");

    assert_eq!(state.requests(), after_first, "nothing was sent");
    assert!(
        started.elapsed() < COOLDOWN_SEARCH_BUDGET / 2,
        "{:?}",
        started.elapsed()
    );
    assert_eq!(page.freshness, RegistryFreshness::LocalFallback);
    assert_eq!(names_of(&page), ["com.notion/mcp"]);
}

#[tokio::test]
async fn a_search_matching_no_cached_detail_reports_the_typed_error() {
    let store = store_with_details(&["com.notion/mcp"]).await;
    let (base, state) = switchable_registry().await;
    let auth = auth_at(&base);
    let adapter = adapter_with_cooldown(Duration::from_secs(60));
    state.set(STALLED);

    let error = adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("no cached detail matches");
    assert_eq!(error.wire_name(), tinymcp_bus::errors::REGISTRY_TIMEOUT);

    let error = adapter
        .search(&store, &auth, &cursors(), "nonesuch", 1, 20)
        .await
        .expect_err("still nothing matches while cooling down");
    assert_eq!(error.wire_name(), tinymcp_bus::errors::REGISTRY_TIMEOUT);
}

#[test]
fn a_server_cached_as_a_page_row_and_a_detail_appears_once() {
    let store = store();
    store
        .cache(
            &search_cache_key("", 1, 20),
            &json!({ "servers": [envelope("com.notion/mcp")] }).to_string(),
        )
        .unwrap();
    cache_detail(&store, &envelope("com.notion/mcp")["server"]);

    let page = super::fallback::serve_cached(&store, &cursors(), "notion", 1, 20).unwrap();

    assert_eq!(names_of(&page), ["com.notion/mcp"]);
}

#[test]
fn a_detail_matching_on_its_name_leads_a_page_row_matching_on_its_description() {
    let store = store();
    store
        .cache(
            &search_cache_key("", 1, 20),
            &json!({ "servers": [{
                "server": {
                    "name": "io.github.acme/pages",
                    "description": "Sync pages to Notion",
                    "packages": [{ "registryType": "npm", "identifier": "pages" }],
                },
            }] })
            .to_string(),
        )
        .unwrap();
    cache_detail(&store, &envelope("com.notion/mcp")["server"]);

    let page = super::fallback::serve_cached(&store, &cursors(), "notion", 1, 20).unwrap();

    assert_eq!(names_of(&page), ["com.notion/mcp", "io.github.acme/pages"]);
}

#[test]
fn a_detail_offering_no_way_to_connect_is_not_matched() {
    let store = store();
    cache_detail(&store, &json!({ "name": "io.example/zzq" }));
    store
        .cache(
            &format!("{}io.example/zzq-broken", super::DETAIL_CACHE_PREFIX),
            r#""not a detail""#,
        )
        .unwrap();

    assert!(super::fallback::serve_cached(&store, &cursors(), "zzq", 1, 20).is_none());
}

#[tokio::test]
async fn a_stalled_search_with_nothing_cached_still_finds_a_curated_server() {
    let (base, state) = switchable_registry().await;
    state.set(STALLED);

    let page = adapter_with_cooldown(Duration::from_secs(60))
        .search(&store(), &auth_at(&base), &cursors(), "slack", 1, 20)
        .await
        .expect("the curated entry stands in");

    assert_eq!(page.freshness, RegistryFreshness::LocalFallback);
    assert_eq!(names_of(&page), ["com.slack/mcp"]);
}

#[test]
fn a_curated_server_leads_cached_rows_in_a_local_match() {
    let store = store();
    store
        .cache(
            &search_cache_key("", 1, 20),
            &json!({ "servers": [{
                "server": {
                    "name": "io.github.acme/notion-sync",
                    "description": "Sync pages",
                    "packages": [{ "registryType": "npm", "identifier": "notion-sync" }],
                },
            }] })
            .to_string(),
        )
        .unwrap();

    let page = super::fallback::serve_cached(&store, &cursors(), "notion", 1, 20).unwrap();

    assert_eq!(
        names_of(&page),
        ["com.notion/mcp", "io.github.acme/notion-sync"]
    );
}

#[test]
fn local_matches_from_details_are_capped_at_the_page_size() {
    let store = store();
    for name in ["com.notion/a", "com.notion/b", "com.notion/c"] {
        cache_detail(&store, &envelope(name)["server"]);
    }

    let page = super::fallback::serve_cached(&store, &cursors(), "notion", 1, 2).unwrap();

    assert_eq!(page.servers.len(), 2);
}

fn summaries(names: &[&str]) -> Vec<tinymcp_bus::RegistryServerSummary> {
    list_response(
        &Value::Array(names.iter().map(|name| envelope(name)).collect()),
        None,
    )
    .into_summaries()
}

fn names(rows: &[tinymcp_bus::RegistryServerSummary]) -> Vec<&str> {
    rows.iter().map(|row| row.qualified_name.as_str()).collect()
}

#[tokio::test]
async fn a_live_search_leads_with_the_curated_server_the_registry_left_out() {
    let (base, _state) = switchable_registry().await;

    let page = adapter()
        .search(&store(), &auth_at(&base), &cursors(), "slack", 1, 20)
        .await
        .expect("the registry answers");

    assert_eq!(page.freshness, RegistryFreshness::Live);
    assert_eq!(names_of(&page), ["com.slack/mcp", "@acme/slack"]);
}

#[test]
fn a_curated_server_already_on_the_page_moves_to_the_head_as_the_registry_row() {
    let mut rows = summaries(&["@acme/notion", "com.notion/mcp"]);
    rows[1].display_name = "From the registry".to_string();

    lead_with_curated(&mut rows, "notion", 20);

    assert_eq!(names(&rows), ["com.notion/mcp", "@acme/notion"]);
    assert_eq!(rows[0].display_name, "From the registry");
}

#[test]
fn a_curated_lead_stays_within_the_page_size() {
    let mut rows = summaries(&["@acme/slack", "@acme/slack-2"]);

    lead_with_curated(&mut rows, "slack", 2);

    assert_eq!(names(&rows), ["com.slack/mcp", "@acme/slack"]);
}

#[test]
fn a_blank_query_leaves_the_page_alone() {
    let mut rows = summaries(&["@acme/one", "@acme/two"]);

    lead_with_curated(&mut rows, "  ", 20);

    assert_eq!(names(&rows), ["@acme/one", "@acme/two"]);
}

#[test]
fn a_query_matching_no_curated_server_leaves_the_page_alone() {
    let mut rows = summaries(&["@acme/one"]);

    lead_with_curated(&mut rows, "nonesuch", 20);

    assert_eq!(names(&rows), ["@acme/one"]);
}
