//! Unit tests for the configuration payload types.
//!
//! These pin the **wire form**. A host and a module that disagree about a field
//! name or a tag value fail at runtime with a decode error and no compiler
//! anywhere will have said a word, so the assertions below are deliberately
//! literal about the JSON rather than round-tripping through the Rust types and
//! calling that proof.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{
    HttpHeader, McpAuthConfig, McpClientConfig, McpClientIdentityConfig, McpProxyConfig,
    McpRegistryAuthConfig, McpServerConfig,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// McpAuthConfig — the tagged enum, where a wire-form mistake is most likely
// ---------------------------------------------------------------------------

#[test]
fn auth_none_serializes_with_its_snake_case_tag() {
    assert_eq!(
        serde_json::to_value(McpAuthConfig::None).expect("None serializes"),
        json!({ "kind": "none" })
    );
}

#[test]
fn auth_bearer_token_serializes_with_its_tag_and_field() {
    let auth = McpAuthConfig::BearerToken {
        token: "abc".into(),
    };
    assert_eq!(
        serde_json::to_value(&auth).expect("bearer serializes"),
        json!({ "kind": "bearer_token", "token": "abc" })
    );
}

#[test]
fn auth_basic_serializes_with_both_credentials() {
    let auth = McpAuthConfig::Basic {
        username: "u".into(),
        password: "p".into(),
    };
    assert_eq!(
        serde_json::to_value(&auth).expect("basic serializes"),
        json!({ "kind": "basic", "username": "u", "password": "p" })
    );
}

#[test]
fn auth_header_serializes_with_its_name_and_value() {
    let auth = McpAuthConfig::Header {
        name: "X-Key".into(),
        value: "v".into(),
    };
    assert_eq!(
        serde_json::to_value(&auth).expect("header serializes"),
        json!({ "kind": "header", "name": "X-Key", "value": "v" })
    );
}

#[test]
fn auth_headers_serializes_as_a_list_of_name_value_pairs() {
    let auth = McpAuthConfig::Headers {
        headers: vec![HttpHeader::new("A", "1"), HttpHeader::new("B", "2")],
    };
    assert_eq!(
        serde_json::to_value(&auth).expect("headers serializes"),
        json!({
            "kind": "headers",
            "headers": [
                { "name": "A", "value": "1" },
                { "name": "B", "value": "2" },
            ],
        })
    );
}

#[test]
fn auth_query_param_serializes_with_its_name_and_value() {
    let auth = McpAuthConfig::QueryParam {
        name: "key".into(),
        value: "v".into(),
    };
    assert_eq!(
        serde_json::to_value(&auth).expect("query param serializes"),
        json!({ "kind": "query_param", "name": "key", "value": "v" })
    );
}

#[test]
fn every_auth_variant_round_trips() {
    let variants = [
        McpAuthConfig::None,
        McpAuthConfig::BearerToken { token: "t".into() },
        McpAuthConfig::Basic {
            username: "u".into(),
            password: "p".into(),
        },
        McpAuthConfig::Header {
            name: "n".into(),
            value: "v".into(),
        },
        McpAuthConfig::Headers {
            headers: vec![HttpHeader::new("n", "v")],
        },
        McpAuthConfig::QueryParam {
            name: "n".into(),
            value: "v".into(),
        },
    ];
    for variant in variants {
        let encoded = serde_json::to_value(&variant).expect("variant serializes");
        let decoded: McpAuthConfig = serde_json::from_value(encoded).expect("variant decodes");
        assert_eq!(decoded, variant);
    }
}

#[test]
fn auth_defaults_to_none() {
    assert_eq!(McpAuthConfig::default(), McpAuthConfig::None);
}

// ---------------------------------------------------------------------------
// McpServerConfig
// ---------------------------------------------------------------------------

#[test]
fn a_server_decodes_from_the_empty_object_and_takes_every_default() {
    let server: McpServerConfig = serde_json::from_value(json!({})).expect("empty object decodes");
    assert_eq!(server, McpServerConfig::default());
    assert!(server.enabled, "a server is enabled unless turned off");
    assert_eq!(server.timeout_secs, 30);
    assert_eq!(server.auth, McpAuthConfig::None);
}

#[test]
fn a_server_decodes_from_the_field_names_a_host_writes() {
    let server: McpServerConfig = serde_json::from_value(json!({
        "name": "weather",
        "endpoint": "https://example.test/mcp",
        "command": "npx",
        "args": ["-y", "weather-mcp"],
        "env": { "API_KEY": "k" },
        "cwd": "/tmp",
        "description": "Weather lookups",
        "enabled": false,
        "allowed_tools": ["forecast"],
        "disallowed_tools": ["debug"],
        "timeout_secs": 5,
        "auth": { "kind": "bearer_token", "token": "t" },
    }))
    .expect("a fully populated server decodes");

    assert_eq!(server.name, "weather");
    assert_eq!(server.endpoint, "https://example.test/mcp");
    assert_eq!(server.command, "npx");
    assert_eq!(server.args, ["-y", "weather-mcp"]);
    assert_eq!(server.env.get("API_KEY").map(String::as_str), Some("k"));
    assert_eq!(server.cwd.as_deref(), Some("/tmp"));
    assert_eq!(server.description.as_deref(), Some("Weather lookups"));
    assert!(!server.enabled);
    assert_eq!(server.allowed_tools, ["forecast"]);
    assert_eq!(server.disallowed_tools, ["debug"]);
    assert_eq!(server.timeout_secs, 5);
    assert_eq!(
        server.auth,
        McpAuthConfig::BearerToken { token: "t".into() }
    );
}

#[test]
fn server_env_serializes_in_a_stable_order() {
    // The ordered map is the reason this holds. An unordered one would make the
    // serialized form depend on hash iteration order, so a byte-comparing
    // consumer — a cache key, a signature, a fixture — would see spurious
    // changes.
    let mut server = McpServerConfig::default();
    server.env.insert("ZULU".into(), "1".into());
    server.env.insert("ALPHA".into(), "2".into());
    server.env.insert("MIKE".into(), "3".into());

    let encoded = serde_json::to_string(&server.env).expect("env serializes");
    assert_eq!(encoded, r#"{"ALPHA":"2","MIKE":"3","ZULU":"1"}"#);
}

// ---------------------------------------------------------------------------
// McpClientConfig
// ---------------------------------------------------------------------------

#[test]
fn a_client_config_decodes_from_the_empty_object() {
    let config: McpClientConfig = serde_json::from_value(json!({})).expect("empty object decodes");
    assert!(config.enabled);
    assert!(config.servers.is_empty());
    assert_eq!(config.proxy, None);
    assert_eq!(config.registry_auth, McpRegistryAuthConfig::default());
}

#[test]
fn a_client_config_round_trips_through_json() {
    let mut config = McpClientConfig::default();
    config.servers.push(McpServerConfig {
        name: "weather".into(),
        endpoint: "https://example.test/mcp".into(),
        ..McpServerConfig::default()
    });
    config.proxy = Some(McpProxyConfig {
        https_proxy: Some("http://proxy.test:3128".into()),
        no_proxy: vec!["localhost".into()],
        ..McpProxyConfig::default()
    });

    let encoded = serde_json::to_value(&config).expect("config serializes");
    let decoded: McpClientConfig = serde_json::from_value(encoded).expect("config decodes");
    assert_eq!(decoded, config);
}

#[test]
fn an_absent_proxy_means_connect_directly() {
    let config: McpClientConfig =
        serde_json::from_value(json!({ "proxy": null })).expect("a null proxy decodes");
    assert_eq!(config.proxy, None);
}

// ---------------------------------------------------------------------------
// McpClientIdentityConfig
// ---------------------------------------------------------------------------

#[test]
fn identity_defaults_name_this_client_library() {
    let identity = McpClientIdentityConfig::default();
    assert_eq!(identity.name, "tinymcp");
    assert_eq!(identity.title, "TinyMCP Client");
    assert_eq!(identity.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn identity_decodes_partially_and_defaults_the_rest() {
    let identity: McpClientIdentityConfig =
        serde_json::from_value(json!({ "name": "openhuman-core" }))
            .expect("a partial identity decodes");
    assert_eq!(identity.name, "openhuman-core");
    assert_eq!(identity.title, "TinyMCP Client");
}

// ---------------------------------------------------------------------------
// McpRegistryAuthConfig
// ---------------------------------------------------------------------------

#[test]
fn registry_auth_defaults_to_every_field_unset() {
    let auth = McpRegistryAuthConfig::default();
    assert_eq!(auth.smithery_api_key, None);
    assert_eq!(auth.mcp_official_base, None);
    assert_eq!(auth.mcp_official_token, None);
}

#[test]
fn redacting_registry_auth_drops_both_secrets_and_reports_that_they_were_set() {
    let auth = McpRegistryAuthConfig {
        smithery_api_key: Some("smithery-secret".into()),
        mcp_official_base: Some("https://registry.test".into()),
        mcp_official_token: Some("official-secret".into()),
    };

    let (redacted, smithery_set, official_set) = auth.redacted();

    assert!(smithery_set);
    assert!(official_set);
    assert_eq!(redacted.smithery_api_key, None);
    assert_eq!(redacted.mcp_official_token, None);
    // The endpoint is not a secret, and a user who cannot see which registry
    // they are pointed at cannot debug it.
    assert_eq!(
        redacted.mcp_official_base.as_deref(),
        Some("https://registry.test")
    );
}

#[test]
fn redacting_unset_registry_auth_reports_both_secrets_absent() {
    let (redacted, smithery_set, official_set) = McpRegistryAuthConfig::default().redacted();
    assert!(!smithery_set);
    assert!(!official_set);
    assert_eq!(redacted, McpRegistryAuthConfig::default());
}

#[test]
fn a_redacted_registry_auth_never_serializes_a_secret() {
    let auth = McpRegistryAuthConfig {
        smithery_api_key: Some("smithery-secret".into()),
        mcp_official_base: None,
        mcp_official_token: Some("official-secret".into()),
    };
    let (redacted, _, _) = auth.redacted();
    let encoded = serde_json::to_string(&redacted).expect("redacted auth serializes");
    assert!(!encoded.contains("smithery-secret"), "{encoded}");
    assert!(!encoded.contains("official-secret"), "{encoded}");
}

// ---------------------------------------------------------------------------
// McpProxyConfig
// ---------------------------------------------------------------------------

#[test]
fn a_proxy_decodes_from_the_empty_object_with_nothing_set() {
    let proxy: McpProxyConfig = serde_json::from_value(json!({})).expect("empty object decodes");
    assert_eq!(proxy, McpProxyConfig::default());
    assert!(proxy.no_proxy.is_empty());
}

#[test]
fn a_proxy_round_trips_every_field() {
    let proxy = McpProxyConfig {
        http_proxy: Some("http://p.test:8080".into()),
        https_proxy: Some("http://p.test:8443".into()),
        all_proxy: Some("socks5://p.test:1080".into()),
        no_proxy: vec!["localhost".into(), ".internal".into()],
    };
    let encoded = serde_json::to_value(&proxy).expect("proxy serializes");
    let decoded: McpProxyConfig = serde_json::from_value(encoded).expect("proxy decodes");
    assert_eq!(decoded, proxy);
}
