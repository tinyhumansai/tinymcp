//! Unit tests for configuration payloads sent to the MCP module.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{
    ConfiguredToolCallRequest, McpAuthConfig, McpClientConfig, McpServerConfig,
    OpenConfiguredRequest, OpenConfiguredResponse, StdioSandboxConfig, StdioSandboxEnforcement,
    TransitionStatus,
};
use serde_json::json;

#[test]
fn configured_open_keeps_directory_and_client_config_together_on_the_wire() {
    let wire = json!({
        "data_dir": "/workspace/agents/alpha",
        "client": {
            "servers": [{
                "name": "alpha-weather",
                "endpoint": "https://alpha.example.test/mcp",
                "auth": { "kind": "bearer_token", "token": "credential" }
            }]
        }
    });

    let request: OpenConfiguredRequest = serde_json::from_value(wire.clone()).unwrap();

    assert_eq!(request.data_dir, "/workspace/agents/alpha");
    assert_eq!(request.client.servers[0].name, "alpha-weather");
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({
            "data_dir": "/workspace/agents/alpha",
            "client": {
                "enabled": true,
                "servers": [{
                    "name": "alpha-weather",
                    "endpoint": "https://alpha.example.test/mcp",
                    "command": "",
                    "args": [],
                    "env": {},
                    "cwd": null,
                    "description": null,
                    "enabled": true,
                    "allowed_tools": [],
                    "disallowed_tools": [],
                    "operator_trusted": false,
                    "timeout_secs": 30,
                    "auth": { "kind": "bearer_token", "token": "credential" }
                }],
                "client_identity": {
                    "name": "tinymcp",
                    "title": "TinyMCP Client",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "registry_auth": {
                    "smithery_api_key": null,
                    "mcp_official_base": null,
                    "mcp_official_token": null
                },
                "proxy": null,
                "stdio_sandbox": null
            }
        })
    );
}

#[test]
fn configured_open_debug_never_exposes_the_user_path_or_credentials() {
    let request = OpenConfiguredRequest {
        data_dir: "/private/user/workspace".into(),
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "private-server".into(),
                auth: McpAuthConfig::BearerToken {
                    token: "sentinel-secret".into(),
                },
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    };

    let debug = format!("{request:?}");

    assert!(debug.contains("OpenConfiguredRequest"));
    assert!(!debug.contains("/private/user/workspace"));
    assert!(!debug.contains("private-server"));
    assert!(!debug.contains("sentinel-secret"));
}

#[test]
fn configured_tool_call_debug_never_exposes_endpoint_auth_or_arguments() {
    let request = ConfiguredToolCallRequest {
        server: McpServerConfig {
            name: "private-server".into(),
            endpoint: "https://user:pass@example.test/mcp?token=url-secret".into(),
            auth: McpAuthConfig::BearerToken {
                token: "sentinel-secret".into(),
            },
            ..McpServerConfig::default()
        },
        tool: "lookup".into(),
        arguments: json!({"query":"argument-secret"}),
    };

    let debug = format!("{request:?}");

    assert!(debug.contains("ConfiguredToolCallRequest"));
    assert!(debug.contains("lookup"));
    for secret in [
        "private-server",
        "user:pass",
        "url-secret",
        "sentinel-secret",
        "argument-secret",
    ] {
        assert!(!debug.contains(secret), "debug output exposed {secret}");
    }
}

#[test]
fn configured_open_returns_an_opaque_cleanup_transition() {
    let response = OpenConfiguredResponse {
        object_path: "/ai/tinyhumans/tinymcp/Dir/d42".into(),
        transition_id: "t17".into(),
    };

    assert_eq!(
        serde_json::to_value(response).unwrap(),
        json!({
            "object_path": "/ai/tinyhumans/tinymcp/Dir/d42",
            "transition_id": "t17"
        })
    );
    assert_eq!(
        serde_json::to_value(TransitionStatus::Pending).unwrap(),
        json!("pending")
    );
    assert_eq!(
        serde_json::to_value(TransitionStatus::Complete).unwrap(),
        json!("complete")
    );
}

#[test]
fn stdio_servers_default_to_untrusted_and_the_client_to_no_sandbox() {
    let config: McpClientConfig = serde_json::from_value(json!({
        "servers": [{ "name": "files", "command": "npx" }]
    }))
    .unwrap();
    assert_eq!(config.stdio_sandbox, None);
    assert!(!config.servers[0].operator_trusted);
    assert_eq!(McpClientConfig::default().stdio_sandbox, None);
}

#[test]
fn the_stdio_sandbox_defaults_to_net_subprocess_and_the_isolation_floor() {
    let sandbox: StdioSandboxConfig =
        serde_json::from_value(json!({ "workspace": "/work/mcp" })).unwrap();
    assert!(sandbox.allow_net && sandbox.allow_subprocess);
    assert!(sandbox.require_process_isolation);
    assert_eq!(sandbox.enforcement, StdioSandboxEnforcement::Enforced);
    let declared: StdioSandboxConfig =
        serde_json::from_value(json!({ "workspace": "/w", "enforcement": "declared" })).unwrap();
    assert_eq!(declared.enforcement, StdioSandboxEnforcement::Declared);
    assert_eq!(
        serde_json::to_value(StdioSandboxEnforcement::Enforced).unwrap(),
        json!("enforced")
    );
    assert!(sandbox.read_only.is_empty() && sandbox.deny_read.is_empty());
    assert!(serde_json::from_value::<StdioSandboxConfig>(json!({})).is_err());
    let trusted: McpServerConfig =
        serde_json::from_value(json!({ "command": "npx", "operator_trusted": true })).unwrap();
    assert!(trusted.operator_trusted);
}
