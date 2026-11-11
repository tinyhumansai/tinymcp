//! Unit tests for the runtime curated catalog.
//!
//! Most of them are about what a runtime entry must *not* get: the official
//! badge and the promotions that go with it. A configuration line is not a
//! vetting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

/// A valid runtime entry named `name`.
fn entry(name: &str) -> CuratedServerEntry {
    CuratedServerEntry {
        qualified_name: name.into(),
        display_name: "Swiggy Food".into(),
        description: "Order food".into(),
        remote_url: "https://mcp.swiggy.com/food".into(),
        transport: CuratedEntryTransport::StreamableHttp,
        auth: CuratedEntryAuth::Oauth,
        icon_url: None,
    }
}

/// A row as an upstream would send it.
fn row(name: &str) -> RegistryServerSummary {
    serde_json::from_value(json!({ "qualified_name": name, "display_name": name })).unwrap()
}

#[test]
fn with_no_runtime_entries_the_catalog_is_the_built_in_list() {
    let catalog = CuratedCatalog::new(&[]);

    assert_eq!(catalog.entries().len(), CURATED_SERVERS.len());
    for server in CURATED_SERVERS {
        assert!(catalog.is_promoted(server.qualified_name));
        assert_eq!(catalog.get(server.qualified_name), Some(&server.to_entry()));
    }
}

#[test]
fn a_runtime_entry_is_listed_but_never_badged_or_promoted() {
    let catalog = CuratedCatalog::new(&[entry("com.example/added")]);

    assert_eq!(catalog.entries().last(), Some(&entry("com.example/added")));
    assert!(catalog.get("com.example/added").is_some());
    assert!(!catalog.is_promoted("com.example/added"));
    assert!(
        catalog
            .promoted()
            .all(|promoted| promoted.qualified_name != "com.example/added")
    );

    let mut rows = vec![row("com.example/added"), row("com.notion/mcp")];
    rows[0].official = true;
    catalog.tag_official(&mut rows);
    assert!(!rows[0].official, "a runtime entry must not keep a badge");
    assert!(rows[1].official);
}

#[test]
fn a_runtime_entry_naming_a_built_in_replaces_it_and_takes_its_badge() {
    let mut moved = entry("com.notion/mcp");
    moved.remote_url = "https://notion.example/mcp".into();
    let catalog = CuratedCatalog::new(&[moved.clone()]);

    assert_eq!(catalog.get("com.notion/mcp"), Some(&moved));
    assert_eq!(
        catalog
            .entries()
            .iter()
            .filter(|curated| curated.qualified_name == "com.notion/mcp")
            .count(),
        1
    );
    assert!(!catalog.is_promoted("com.notion/mcp"));
    assert_eq!(catalog.entries().len(), CURATED_SERVERS.len());

    let mut rows = vec![row("com.notion/mcp"), row("com.stripe/mcp")];
    catalog.tag_official(&mut rows);
    assert!(!rows[0].official, "a replaced built-in loses its badge");
    assert!(rows[1].official, "the other built-ins keep theirs");

    // The replacement is still a valid entry, so validation keeps it.
    assert_eq!(validated_runtime_entries(vec![moved.clone()]), [moved]);
}

#[test]
fn validation_drops_each_bad_entry_and_keeps_the_rest() {
    let good = entry("com.example/good");
    let mut plain_http = entry("com.example/http");
    plain_http.remote_url = "http://mcp.example/".into();
    let mut credentials = entry("com.example/creds");
    credentials.remote_url = "https://user:pass@mcp.example/".into();
    let mut no_host = entry("com.example/nohost");
    no_host.remote_url = "https://".into();
    let mut bad_icon = entry("com.example/icon");
    bad_icon.icon_url = Some("http://cdn.example/icon.png".into());
    let mut blank_name = entry("  ");
    blank_name.qualified_name = "  ".into();
    let mut blank_display = entry("com.example/display");
    blank_display.display_name = String::new();
    let duplicate = entry("com.example/good");
    let mut good_icon = entry("com.example/with-icon");
    good_icon.icon_url = Some("https://cdn.example/icon.png".into());

    let kept = validated_runtime_entries(vec![
        good.clone(),
        plain_http,
        credentials,
        no_host,
        bad_icon,
        blank_name,
        blank_display,
        duplicate,
        good_icon.clone(),
    ]);

    assert_eq!(kept, [good, good_icon]);
}

#[test]
fn a_runtime_entry_becomes_an_unbadged_row_and_a_hosted_detail() {
    let mut token = entry("com.example/token");
    token.auth = CuratedEntryAuth::Token;
    token.transport = CuratedEntryTransport::Sse;
    token.icon_url = Some("https://cdn.example/icon.png".into());

    let summary = entry_summary(&token);
    assert_eq!(summary.qualified_name, "com.example/token");
    assert!(!summary.official);
    assert_eq!(summary.auth_kind.as_deref(), Some("api_key"));
    assert_eq!(
        summary.icon_url.as_deref(),
        Some("https://cdn.example/icon.png")
    );
    assert_eq!(summary.source, SOURCE_MCP_OFFICIAL);

    let detail = entry_detail(&token);
    assert_eq!(detail.connections.len(), 1);
    assert_eq!(detail.connections[0].r#type, "sse");
    assert_eq!(
        detail.connections[0].deployment_url.as_deref(),
        Some("https://mcp.swiggy.com/food")
    );
    assert!(detail.connections[0].config_schema.is_some());

    let open = entry_summary(&entry("com.example/open"));
    assert_eq!(open.auth_kind, None);
    assert!(
        entry_detail(&entry("com.example/open")).connections[0]
            .config_schema
            .is_none()
    );
}

#[test]
fn a_built_in_server_converts_to_the_same_rows_either_way() {
    for server in CURATED_SERVERS {
        assert_eq!(entry_summary(&server.to_entry()), server.to_summary());
        assert_eq!(entry_detail(&server.to_entry()), server.to_detail());
    }
}
