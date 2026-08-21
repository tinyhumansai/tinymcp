//! Unit tests for catalog curation.
//!
//! The badge is a claim made to a user about software nobody here has read, so
//! the tests are mostly about what must *not* be badged: a name that merely
//! contains a vendor's, and a row that arrives already claiming the flag.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{
    OFFICIAL_SERVERS, float_official_first, is_perfect_server, retain_perfect_servers, tag_official,
};
use tinymcp_bus::RegistryServerSummary;

/// A plain catalog row.
fn server(qualified_name: &str) -> RegistryServerSummary {
    serde_json::from_value(serde_json::json!({
        "qualified_name": qualified_name,
        "display_name": qualified_name,
    }))
    .expect("a summary decodes")
}

/// A row that declares everything the strict filter wants.
fn perfect(qualified_name: &str) -> RegistryServerSummary {
    RegistryServerSummary {
        website_url: Some("https://vendor.test".to_string()),
        auth_kind: Some("api_key".to_string()),
        ..server(qualified_name)
    }
}

// ---------------------------------------------------------------------------
// The list itself
// ---------------------------------------------------------------------------

#[test]
fn the_canonical_list_has_no_duplicates() {
    let mut sorted = OFFICIAL_SERVERS.to_vec();
    sorted.sort_unstable();
    let mut deduplicated = sorted.clone();
    deduplicated.dedup();
    assert_eq!(sorted, deduplicated);
}

#[test]
fn every_canonical_name_is_fully_qualified() {
    // A bare vendor word here would badge far more than intended.
    for name in OFFICIAL_SERVERS {
        assert!(name.contains('/'), "{name} is not a qualified name");
        assert!(!name.trim().is_empty());
    }
}

// ---------------------------------------------------------------------------
// tag_official
// ---------------------------------------------------------------------------

#[test]
fn only_an_exact_canonical_name_is_badged() {
    let mut servers = vec![
        server("io.github.github/github-mcp-server"),
        // Contains "github", is not the canonical server.
        server("ai.smithery/Hint-Services-obsidian-github-mcp"),
        server("com.notion/mcp"),
        server("ai.smithery/smithery-notion"),
        // Contains "stripe", is not the canonical server.
        server("io.github.CSOAI-ORG/meok-stripe-acp-checkout-mcp"),
    ];

    tag_official(&mut servers);

    assert!(servers[0].official);
    assert!(
        !servers[1].official,
        "a name merely containing 'github' must not be badged"
    );
    assert!(servers[2].official);
    assert!(!servers[3].official);
    assert!(
        !servers[4].official,
        "a name merely containing 'stripe' must not be badged"
    );
}

#[test]
fn a_row_arriving_with_the_badge_set_has_it_cleared() {
    // The flag is ours to set. A row that claims it is claiming something we
    // did not check.
    let mut servers = vec![RegistryServerSummary {
        official: true,
        ..server("ai.smithery/impostor")
    }];

    tag_official(&mut servers);

    assert!(!servers[0].official);
}

#[test]
fn badging_an_empty_catalog_does_nothing() {
    let mut servers: Vec<RegistryServerSummary> = Vec::new();
    tag_official(&mut servers);
    assert!(servers.is_empty());
}

#[test]
fn a_prefix_of_a_canonical_name_is_not_badged() {
    let mut servers = vec![
        server("com.notion"),
        server("com.notion/mcp-extra"),
        server("prefix-com.notion/mcp"),
    ];

    tag_official(&mut servers);

    assert!(servers.iter().all(|server| !server.official));
}

// ---------------------------------------------------------------------------
// The strict filter
// ---------------------------------------------------------------------------

#[test]
fn a_row_declaring_a_website_and_a_key_is_kept() {
    assert!(is_perfect_server(&perfect("com.acme/mcp")));
}

#[test]
fn a_row_missing_either_signal_is_dropped() {
    let mut servers = vec![
        perfect("com.acme/mcp"),
        RegistryServerSummary {
            auth_kind: None,
            ..perfect("oauth/server")
        },
        RegistryServerSummary {
            website_url: None,
            ..perfect("nosite/server")
        },
        server("community/server"),
    ];

    let dropped = retain_perfect_servers(&mut servers);

    let names: Vec<&str> = servers
        .iter()
        .map(|server| server.qualified_name.as_str())
        .collect();
    assert_eq!(names, ["com.acme/mcp"]);
    assert_eq!(dropped, 3);
}

#[test]
fn a_blank_website_does_not_count_as_declaring_one() {
    for blank in ["", "   ", "\t\n"] {
        let candidate = RegistryServerSummary {
            website_url: Some(blank.to_string()),
            ..perfect("blank/server")
        };
        assert!(!is_perfect_server(&candidate), "{blank:?} was accepted");
    }
}

#[test]
fn an_unrecognised_credential_kind_is_not_accepted() {
    // Only a named static credential is enough to install without guessing.
    let candidate = RegistryServerSummary {
        auth_kind: Some("oauth2".to_string()),
        ..perfect("oauth/server")
    };
    assert!(!is_perfect_server(&candidate));
}

#[test]
fn filtering_an_empty_catalog_drops_nothing() {
    let mut servers: Vec<RegistryServerSummary> = Vec::new();
    assert_eq!(retain_perfect_servers(&mut servers), 0);
}

#[test]
fn filtering_reports_the_full_count_when_it_drops_everything() {
    // A caller logs this. A filter that silently empties a catalog reads to a
    // user as "there is nothing here".
    let mut servers = vec![server("a/one"), server("b/two"), server("c/three")];
    assert_eq!(retain_perfect_servers(&mut servers), 3);
    assert!(servers.is_empty());
}

#[test]
fn a_trust_signal_from_the_wire_cannot_pass_the_filter() {
    // The two fields are `skip_deserializing`. This is the end-to-end check
    // that an upstream cannot filter itself into the catalog.
    let mut servers: Vec<RegistryServerSummary> = serde_json::from_value(serde_json::json!([{
        "qualified_name": "evil/server",
        "display_name": "Evil",
        "website_url": "https://spoofed.test",
        "auth_kind": "api_key",
    }]))
    .expect("a summary decodes");

    let dropped = retain_perfect_servers(&mut servers);

    assert!(servers.is_empty(), "a wire-supplied claim passed curation");
    assert_eq!(dropped, 1);
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

#[test]
fn badged_servers_float_to_the_top_and_the_rest_keep_their_order() {
    let mut servers = vec![
        perfect("a/one"),
        RegistryServerSummary {
            official: true,
            ..perfect("b/official")
        },
        perfect("c/two"),
    ];

    float_official_first(&mut servers);

    let names: Vec<&str> = servers
        .iter()
        .map(|server| server.qualified_name.as_str())
        .collect();
    assert_eq!(names, ["b/official", "a/one", "c/two"]);
}

#[test]
fn several_badged_servers_keep_their_relative_order() {
    // The upstream ranked them; floating must not reshuffle within a group.
    let mut servers = vec![
        perfect("a/plain"),
        RegistryServerSummary {
            official: true,
            ..perfect("b/first-official")
        },
        RegistryServerSummary {
            official: true,
            ..perfect("c/second-official")
        },
    ];

    float_official_first(&mut servers);

    let names: Vec<&str> = servers
        .iter()
        .map(|server| server.qualified_name.as_str())
        .collect();
    assert_eq!(names, ["b/first-official", "c/second-official", "a/plain"]);
}

#[test]
fn floating_a_catalog_with_nothing_badged_changes_nothing() {
    let mut servers = vec![perfect("a/one"), perfect("b/two"), perfect("c/three")];

    float_official_first(&mut servers);

    let names: Vec<&str> = servers
        .iter()
        .map(|server| server.qualified_name.as_str())
        .collect();
    assert_eq!(names, ["a/one", "b/two", "c/three"]);
}
