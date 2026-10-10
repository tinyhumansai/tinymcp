//! Unit tests for the agent-tool contract: argument normalization, the
//! registry tool specs, and the per-action spec builder.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};

use super::{AgentToolEffect, AgentToolSpec, ArgsError, RegistryTool, normalize_tool_arguments};

// ---------------------------------------------------------------------------
// Argument normalization
// ---------------------------------------------------------------------------

#[test]
fn missing_arguments_normalize_to_an_empty_object() {
    assert_eq!(
        normalize_tool_arguments(None).unwrap(),
        serde_json::Map::new()
    );
}

#[test]
fn null_arguments_normalize_to_an_empty_object() {
    assert_eq!(
        normalize_tool_arguments(Value::Null).unwrap(),
        serde_json::Map::new()
    );
}

#[test]
fn an_object_is_passed_through_unchanged() {
    let arguments = json!({ "city": "London", "days": 3, "nested": { "a": [1, 2] } });
    let normalized = normalize_tool_arguments(arguments.clone()).unwrap();
    assert_eq!(Value::Object(normalized), arguments);
}

#[test]
fn a_json_encoded_empty_object_string_is_decoded() {
    // The reported failure: a model sent `"arguments": "{}"`.
    assert_eq!(
        normalize_tool_arguments(json!("{}")).unwrap(),
        serde_json::Map::new()
    );
}

#[test]
fn a_json_encoded_object_string_is_decoded() {
    let normalized = normalize_tool_arguments(json!(r#"{"city":"Paris","days":2}"#)).unwrap();
    assert_eq!(
        Value::Object(normalized),
        json!({ "city": "Paris", "days": 2 })
    );
}

#[test]
fn surrounding_whitespace_on_an_encoded_object_is_tolerated() {
    let normalized = normalize_tool_arguments(json!("  \n{\"a\": 1}\n ")).unwrap();
    assert_eq!(Value::Object(normalized), json!({ "a": 1 }));
}

#[test]
fn a_markdown_fenced_object_string_is_decoded() {
    for fenced in [
        "```json\n{\"a\": 1}\n```",
        "```JSON\n{\"a\": 1}```",
        "```\n{\"a\": 1}\n```",
        "  ```json {\"a\": 1} ```  ",
    ] {
        let normalized = normalize_tool_arguments(json!(fenced))
            .unwrap_or_else(|error| panic!("{fenced:?}: {error}"));
        assert_eq!(Value::Object(normalized), json!({ "a": 1 }), "{fenced:?}");
    }
}

#[test]
fn a_value_of_another_type_is_refused_naming_the_type() {
    let cases = [
        (json!(true), "a boolean"),
        (json!(42), "a number"),
        (json!([1, 2]), "an array"),
    ];
    for (value, actual) in cases {
        let error = normalize_tool_arguments(value.clone()).expect_err("refused");
        assert_eq!(error, ArgsError::NotAnObject { actual }, "{value}");
        assert!(error.to_string().contains(actual), "{error}");
    }
}

#[test]
fn a_string_that_is_not_json_is_refused() {
    let error = normalize_tool_arguments(json!("city=London")).expect_err("refused");
    assert_eq!(error, ArgsError::StringNotAnObject { decoded: None });
    assert!(error.to_string().contains("not JSON"), "{error}");
}

#[test]
fn a_string_holding_json_that_is_not_an_object_is_refused_naming_the_type() {
    let cases = [
        (json!("[1, 2]"), "an array"),
        (json!("\"nested\""), "a string"),
        (json!("7"), "a number"),
        (json!("null"), "null"),
    ];
    for (value, decoded) in cases {
        let error = normalize_tool_arguments(value.clone()).expect_err("refused");
        assert_eq!(
            error,
            ArgsError::StringNotAnObject {
                decoded: Some(decoded)
            },
            "{value}"
        );
        assert!(error.to_string().contains(decoded), "{error}");
    }
}

#[test]
fn an_empty_string_is_refused() {
    let error = normalize_tool_arguments(json!("   ")).expect_err("refused");
    assert_eq!(error, ArgsError::StringNotAnObject { decoded: None });
}

#[test]
fn the_error_is_a_std_error() {
    let error: Box<dyn std::error::Error> = Box::new(ArgsError::NotAnObject { actual: "a number" });
    assert!(
        error
            .to_string()
            .starts_with("tool arguments must be a JSON object")
    );
}

// ---------------------------------------------------------------------------
// Registry tool specs
// ---------------------------------------------------------------------------

/// What each registry tool presents to a model, byte for byte.
///
/// Captured from the hand-written tools this contract replaced. Tool names,
/// descriptions and schemas are prompt-cache and transcript identity: a byte of
/// drift invalidates every cached prefix and changes what a resumed session
/// replays, so the schema is compared as serialized text.
fn registry_goldens() -> Vec<(
    RegistryTool,
    &'static str,
    &'static str,
    &'static str,
    AgentToolEffect,
    bool,
)> {
    vec![
        (
            RegistryTool::Search,
            "mcp_registry_search",
            r#"Search the MCP server registry catalog by `query`, optionally filtered by `transport` ("stdio" | "hosted" | "all"), paginated by `page` / `page_size`. Use to discover installable MCP servers."#,
            r#"{"properties":{"page":{"minimum":1,"type":"integer"},"page_size":{"minimum":1,"type":"integer"},"query":{"type":"string"},"transport":{"enum":["stdio","hosted","all"],"type":"string"}},"type":"object"}"#,
            AgentToolEffect::Read,
            true,
        ),
        (
            RegistryTool::Get,
            "mcp_registry_get",
            "Get one MCP registry server's detail by `qualified_name`.",
            r#"{"properties":{"qualified_name":{"type":"string"}},"required":["qualified_name"],"type":"object"}"#,
            AgentToolEffect::Read,
            true,
        ),
        (
            RegistryTool::InstalledList,
            "mcp_registry_installed_list",
            "List the MCP servers currently installed for this user.",
            r#"{"properties":{},"type":"object"}"#,
            AgentToolEffect::Read,
            true,
        ),
        (
            RegistryTool::Status,
            "mcp_registry_status",
            "Report the connection status of installed MCP servers.",
            r#"{"properties":{},"type":"object"}"#,
            AgentToolEffect::Read,
            false,
        ),
        (
            RegistryTool::ListTools,
            "mcp_registry_list_tools",
            "List the tools (name, description, input schema) exposed by a connected MCP server, given its `server_id`. Use this to discover what a connected server can do before calling `mcp_registry_tool_call`. The server must already be connected (see `mcp_registry_status` / `mcp_registry_connect`).",
            r#"{"properties":{"server_id":{"type":"string"}},"required":["server_id"],"type":"object"}"#,
            AgentToolEffect::Read,
            false,
        ),
        (
            RegistryTool::Connect,
            "mcp_registry_connect",
            "Connect (spawn + handshake) an installed MCP server by `server_id`, returning its tools.",
            r#"{"properties":{"server_id":{"type":"string"}},"required":["server_id"],"type":"object"}"#,
            AgentToolEffect::Execute,
            false,
        ),
        (
            RegistryTool::Disconnect,
            "mcp_registry_disconnect",
            "Disconnect (stop) a connected MCP server by `server_id`.",
            r#"{"properties":{"server_id":{"type":"string"}},"required":["server_id"],"type":"object"}"#,
            AgentToolEffect::Execute,
            false,
        ),
        (
            RegistryTool::ToolCall,
            "mcp_registry_tool_call",
            "Invoke a tool on a connected MCP server: `server_id` + `tool_name` + `arguments` object.",
            r#"{"properties":{"arguments":{"type":"object"},"server_id":{"type":"string"},"tool_name":{"type":"string"}},"required":["server_id","tool_name"],"type":"object"}"#,
            AgentToolEffect::Execute,
            false,
        ),
        (
            RegistryTool::Uninstall,
            "mcp_registry_uninstall",
            "Uninstall an installed MCP server by `server_id`. Default-OFF (opt-in).",
            r#"{"properties":{"server_id":{"type":"string"}},"required":["server_id"],"type":"object"}"#,
            AgentToolEffect::Write,
            false,
        ),
    ]
}

#[test]
fn every_registry_tool_is_listed_once_in_registration_order() {
    let names: Vec<&str> = RegistryTool::ALL.iter().map(|tool| tool.name()).collect();
    let expected: Vec<&str> = registry_goldens().iter().map(|row| row.1).collect();
    assert_eq!(names, expected);
}

#[test]
fn registry_specs_carry_the_exact_identity_the_host_shipped() {
    for (tool, name, description, schema, effect, deferred) in registry_goldens() {
        let spec = tool.spec();
        assert_eq!(spec.name, name);
        assert_eq!(tool.name(), name);
        assert_eq!(spec.description, description, "{name}");
        assert_eq!(
            serde_json::to_string(&spec.parameters).unwrap(),
            schema,
            "{name}"
        );
        assert_eq!(spec.effect, effect, "{name}");
        assert_eq!(spec.deferred, deferred, "{name}");
    }
}

#[test]
fn registry_specs_come_back_in_registration_order() {
    let specs = super::registry_tool_specs();
    let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
    let expected: Vec<&str> = RegistryTool::ALL.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, expected);
}

#[test]
fn a_registry_tool_is_found_by_its_name() {
    for tool in RegistryTool::ALL {
        assert_eq!(RegistryTool::from_name(tool.name()), Some(tool));
    }
    assert_eq!(RegistryTool::from_name("mcp_registry_install"), None);
}

#[test]
fn the_tool_call_schema_still_advertises_an_object() {
    // Normalization is tolerance at execution; the schema a model reads is
    // unchanged, so a well-behaved model keeps sending an object.
    let spec = RegistryTool::ToolCall.spec();
    assert_eq!(
        spec.parameters["properties"]["arguments"],
        json!({ "type": "object" })
    );
}

#[test]
fn a_spec_pins_its_wire_form() {
    let spec = AgentToolSpec {
        name: "t".into(),
        description: "d".into(),
        parameters: json!({ "type": "object" }),
        effect: AgentToolEffect::Execute,
        deferred: true,
    };
    let encoded = serde_json::to_value(&spec).unwrap();
    assert_eq!(
        encoded,
        json!({
            "name": "t",
            "description": "d",
            "parameters": { "type": "object" },
            "effect": "execute",
            "deferred": true,
        })
    );
    let decoded: AgentToolSpec = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, spec);
}

#[test]
fn effects_serialize_in_snake_case() {
    for (effect, wire) in [
        (AgentToolEffect::Read, "read"),
        (AgentToolEffect::Execute, "execute"),
        (AgentToolEffect::Write, "write"),
    ] {
        assert_eq!(serde_json::to_value(effect).unwrap(), json!(wire));
    }
}

// ---------------------------------------------------------------------------
// Call outcome
// ---------------------------------------------------------------------------

#[test]
fn the_call_outcome_kind_is_pinned() {
    assert_eq!(tinymcp_bus::MCP_CALL_RESULT_KIND, "mcp_call");
}

#[test]
fn an_answered_outcome_pins_its_wire_form_and_omits_the_error() {
    let outcome = super::McpCallOutcome::answered("docs", "search");
    let wire = serde_json::to_value(&outcome).unwrap();
    assert_eq!(
        wire,
        json!({ "kind": "mcp_call", "server": "docs", "tool": "search", "ok": true })
    );
    assert_eq!(
        serde_json::from_value::<super::McpCallOutcome>(wire).unwrap(),
        outcome
    );
}

#[test]
fn a_failed_outcome_pins_its_wire_form_and_round_trips() {
    let outcome = super::McpCallOutcome::failed(
        "docs",
        "search",
        super::McpCallError {
            code: tinymcp_bus::errors::UNAUTHORIZED.into(),
            unauthorized: true,
            advertises_oauth: true,
        },
    );
    let wire = serde_json::to_value(&outcome).unwrap();
    assert_eq!(
        wire,
        json!({
            "kind": "mcp_call",
            "server": "docs",
            "tool": "search",
            "ok": false,
            "error": {
                "code": "ai.tinyhumans.tinymcp.Error.Unauthorized",
                "unauthorized": true,
                "advertises_oauth": true,
            },
        })
    );
    assert_eq!(
        serde_json::from_value::<super::McpCallOutcome>(wire).unwrap(),
        outcome
    );
}

#[test]
fn a_plain_error_carries_no_authorization_signal() {
    let error = super::McpCallError::new(tinymcp_bus::errors::TRANSPORT);
    assert_eq!(error.code, tinymcp_bus::errors::TRANSPORT);
    assert!(!error.unauthorized);
    assert!(!error.advertises_oauth);
}

#[test]
fn an_outcome_is_read_back_only_from_metadata_of_its_kind() {
    let outcome = super::McpCallOutcome::answered("docs", "search");
    let metadata = serde_json::to_value(&outcome).unwrap();
    assert_eq!(
        super::McpCallOutcome::from_metadata(&metadata),
        Some(outcome)
    );

    assert_eq!(
        super::McpCallOutcome::from_metadata(&json!({ "kind": "web_search" })),
        None
    );
    assert_eq!(super::McpCallOutcome::from_metadata(&json!([1, 2])), None);
    assert_eq!(
        super::McpCallOutcome::from_metadata(&json!({ "kind": "mcp_call", "ok": "yes" })),
        None
    );
}

#[test]
fn decoding_an_outcome_rejects_what_the_constructors_never_produce() {
    let decode = |value: serde_json::Value| serde_json::from_value::<super::McpCallOutcome>(value);
    let error = json!({ "code": tinymcp_bus::errors::TRANSPORT, "unauthorized": false, "advertises_oauth": false });
    let base =
        |ok: bool| json!({ "kind": "mcp_call", "server": "docs", "tool": "search", "ok": ok });

    let mut answered_with_error = base(true);
    answered_with_error["error"] = error.clone();
    assert!(decode(answered_with_error).is_err());

    assert!(decode(base(false)).is_err());
    let mut failed_null_error = base(false);
    failed_null_error["error"] = serde_json::Value::Null;
    assert!(decode(failed_null_error).is_err());

    let mut missing_kind = base(true);
    missing_kind.as_object_mut().unwrap().remove("kind");
    assert!(decode(missing_kind).is_err());
    let mut wrong_kind = base(true);
    wrong_kind["kind"] = json!("web_search");
    assert!(decode(wrong_kind).is_err());

    assert_eq!(
        decode(base(true)).unwrap(),
        super::McpCallOutcome::answered("docs", "search")
    );
    let mut failed = base(false);
    failed["error"] = error;
    assert_eq!(
        decode(failed).unwrap(),
        super::McpCallOutcome::failed(
            "docs",
            "search",
            super::McpCallError::new(tinymcp_bus::errors::TRANSPORT)
        )
    );
}

#[test]
fn decoding_an_error_rejects_an_oauth_advert_without_a_401() {
    let decode = |unauthorized: bool, advertises_oauth: bool| {
        serde_json::from_value::<super::McpCallError>(json!({
            "code": "x",
            "unauthorized": unauthorized,
            "advertises_oauth": advertises_oauth,
        }))
    };
    assert!(decode(false, true).is_err());
    assert!(decode(false, false).is_ok());
    assert!(decode(true, false).is_ok());
    assert!(decode(true, true).is_ok());

    let nested = json!({
        "kind": "mcp_call", "server": "docs", "tool": "t", "ok": false,
        "error": { "code": "x", "unauthorized": false, "advertises_oauth": true },
    });
    assert_eq!(super::McpCallOutcome::from_metadata(&nested), None);
}
